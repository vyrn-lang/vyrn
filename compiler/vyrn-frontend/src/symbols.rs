//! Symbol-query API for the LSP: a non-invasive layer over the parsed
//! [`crate::ast::Program`] and the lexer's per-token positions.
//!
//! The AST carries only a 1-based `line` per node (no column/span), and
//! identifiers are bare `String`. Rather than re-thread spans through the parser
//! (high churn across every node construction site), this module reuses the
//! lexer's per-token `(line, col)` — already on `Token` for diagnostics — to give
//! every top-level declaration a precise name column, and to map a cursor
//! position to the identifier token under it. Top-level names (functions, types,
//! variants) are unique, so name-based resolution is robust here; locals/params
//! (where shadowing lives) are deferred.
//!
//! [`analyze`] runs the whole pipeline once per document (lex → parse → check →
//! movecheck) and returns diagnostics + a symbol index + the identifier tokens.
//! `vyrn_frontend::diagnostics` delegates to it, so there is a single pipeline.
//! The LSP calls [`analyze`] on open/change and serves hover/go-to-def/completion
//! from the cached [`Analysis`].

use crate::ast::{
    self, Block, Capability, EnumVariant, Expr, Function, GlobalDecl, MethodSig, ProtocolDecl,
    Stmt, Type, TypeDecl,
};
use crate::checker;
use crate::diagnostics::Diagnostic;
use crate::lexer::{self, Tok};
use crate::movecheck;
use crate::parser;
use crate::symbolmap::MappedSymbol;

/// Kind of a top-level symbol or a local binding (returned by [`resolve`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Type,
    Variant,
    Method,
    /// A record field (member completion only — fields are not standalone
    /// declarations, so they never appear in the symbol index).
    Field,
    /// A function parameter.
    Param,
    /// A `let` binding or a `for`-in loop variable, local to a function body.
    Local,
    /// A top-level module-state binding (RFC-0013): `let [mut] name = init`.
    Global,
}

/// A top-level declaration the LSP can hover / jump to / complete.
#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    /// 1-based declaration line.
    pub line: usize,
    /// 1-based name column (0 = unknown; whole-line fallback).
    pub col: usize,
    /// 1-based column just past the name (exclusive); 0 = unknown.
    pub end_col: usize,
    /// Hover / signature text.
    pub detail: String,
    /// RFC-0051 §1: the declaration's `///` documentation (markdown), as the
    /// parser attached it — rendered beneath the signature on hover. `None` for
    /// declarations without a doc comment and for kinds the AST carries no doc
    /// on (enum variants, protocol method signatures).
    pub doc: Option<String>,
    /// The module file this symbol was declared in — `None` for the open
    /// document itself, `Some(path)` for a symbol imported from another file
    /// (only populated by [`analyze_linked`]). Foreign symbols have `col == 0`
    /// (their token columns belong to the other file's token stream).
    pub file: Option<String>,
}

/// An identifier token's source range, cached for cursor → token mapping.
#[derive(Debug, Clone)]
pub struct TokenInfo {
    pub text: String,
    pub line: usize,
    pub col: usize,
    pub end_col: usize,
}

/// A local binding — a parameter, a `let`, or a `for`-in variable — scoped to a
/// single function body. Indexed for hover/go-to-definition on variables (the
/// most common thing to hover). Reuses the lexer's token column for the name
/// position, exactly like [`Symbol`]; no AST span threading.
#[derive(Debug, Clone)]
pub struct LocalBinding {
    pub name: String,
    pub kind: LocalKind,
    /// Declared type, if any. `None` for unannotated `let`s and `for`-in vars
    /// (the element type is inferred by the checker and not retained here).
    pub ty: Option<Type>,
    /// 1-based definition line. For a param this is the function's line; for a
    /// `let`/`for` it is the statement's line.
    pub line: usize,
    /// 1-based name column (0 = unknown).
    pub col: usize,
    /// 1-based name end column (0 = unknown).
    pub end_col: usize,
    /// The enclosing function's declaration line (scopes the binding).
    pub fn_line: usize,
}

/// The flavor of a [`LocalBinding`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalKind {
    /// A function parameter (`fn area(s: Shape)` → `s`).
    Param,
    /// `let [mut] name [: Type] = value;` (annotated or not).
    Let { mutable: bool },
    /// `for name in iter { .. }` — the loop variable.
    ForVar,
}

/// Everything the LSP needs for one document, built in a single pass.
#[derive(Debug, Clone)]
pub struct Analysis {
    pub diagnostics: Vec<Diagnostic>,
    pub symbols: Vec<Symbol>,
    pub tokens: Vec<TokenInfo>,
    /// Local bindings (params/lets/for-vars) per function, for variable hover
    /// and go-to-definition.
    pub locals: Vec<LocalBinding>,
    /// Sorted top-level declaration lines (functions, types, protocols, impls) —
    /// used to bound a function's line range for cursor→enclosing-function.
    pub decl_lines: Vec<usize>,
    /// Sorted subset of [`Self::decl_lines`] that are function declarations
    /// (functions + impl methods), so a cursor inside a type/protocol decl is
    /// not mistaken for being in the preceding function.
    pub fn_lines: Vec<usize>,
    /// User protocol methods per implementing type (`impl P for T` → T's
    /// methods), for `.foo` member completion on a concrete receiver. Indexed
    /// from the linked program when available, so imported impls count.
    pub impl_members: Vec<(Type, Completion)>,
    /// Each protocol's methods by protocol name, for `.foo` member completion
    /// on a bounded generic receiver (`fn f<T: Show>(x: T)` → `x.` offers
    /// `Show`'s methods).
    pub protocol_members: Vec<(String, Completion)>,
    /// Per-function type-parameter bounds: `(fn decl line, type param, bound
    /// names)` — how a `Named("T")` receiver finds its protocols.
    pub type_param_bounds: Vec<(usize, String, Vec<String>)>,
    /// Record fields by declaring type name, for `.foo` member completion on a
    /// record receiver (`u: User` → `u.` offers `age`). Refined fields render
    /// as written (`age: Int64 where value >= 18`).
    pub record_fields: Vec<(String, Completion)>,
    /// RFC-0020 M1: for every **finite** validated string type, its full
    /// enumerated language (up to a cap). Powers string-literal completion —
    /// `t("` offers every `TransKey`. A type with more than the cap members (or
    /// an infinite / non-regex type) is absent, so the LSP simply offers nothing.
    pub finite_string_types: Vec<(String, Vec<String>)>,
    /// Top-level function name → parameter types, so string-literal completion
    /// can find the expected type at a call argument position.
    pub fn_param_types: Vec<(String, Vec<Type>)>,
    /// RFC-0042: for every *sequence* validated string type — one whose language is
    /// an infinite space-separated sequence over a finite alphabet (`Tw` =
    /// `token( token)*`) — its enumerated single-token **alphabet** (`= L(TwClass)`).
    /// Powers `class="…"` / `theme.cls("…")` token-in-sequence completion: the
    /// alphabet is derived from the same DFA the compiler checks against.
    pub sequence_string_types: Vec<(String, Vec<String>)>,
    /// RFC-0042: the theme's utility stylesheet — the constant `std/tw` `css()`
    /// returns — captured when a sequence string type (a `Tw`) is present, so hover
    /// on a class token can show the exact CSS rule `css()` emits (utilities) or
    /// report "safelisted (app-styled)" (no rule). Empty when no theme is linked.
    pub tw_css: String,
    /// RFC-0027: each `import * as ns` binding and the exported symbols reachable
    /// through it, so `ns.` completes and `ns.member` hovers / jumps into the
    /// source module. Populated only by [`analyze_linked`].
    pub namespaces: Vec<NamespaceInfo>,
    /// RFC-0033: the origin directive tables of every synthesized generator
    /// module reachable from this document, for forward hover/completion/
    /// go-to-definition inside generator input files. Empty without a linker or
    /// when no generator emitted `//@origin` directives.
    pub origins: crate::origin::OriginMaps,
    /// RFC-0033: diagnostics relocated to a generator input file (`.vyx`, …).
    /// The LSP publishes these against the input file's URI so template errors
    /// appear in that buffer; they are excluded from [`Self::diagnostics`] (which
    /// stays anchored to the open document).
    pub remapped: Vec<Diagnostic>,
    /// RFC-0073 M3: every symbol the generated modules reachable from this
    /// document map back to a declaration, with its derived wire facts. The LSP
    /// reads it to answer "what is this procedure mounted at" on the
    /// DECLARATION's own hover — the declaration's file reaches no generator, so
    /// the facts can only come from a root that does.
    pub symbol_maps: Vec<crate::symbolmap::MappedSymbol>,
    /// RFC-0087 U1: what the ownership analysis decided about every `let` in
    /// THIS document — is it reclaimed, and if not, why. The answer
    /// `vyrn why --memory` prints, at the cursor.
    ///
    /// The model was invisible, and the census ranks that as the root usability
    /// gap: a user cannot learn a rule they cannot observe. Read from
    /// [`crate::own::analyze`], which reads from the walk that DECIDED — never
    /// re-derived, because a second walk could disagree with the first.
    ///
    /// Empty when the checks did not run (a parse error, a failed link).
    pub memory: Vec<MemoryNote>,
}

/// One binding's memory answer, positioned for the editor (RFC-0087 U1).
///
/// [`crate::own::BindingNote`] with the prose already rendered, so the LSP is an
/// adapter rather than a second opinion.
#[derive(Debug, Clone)]
pub struct MemoryNote {
    pub name: String,
    /// 1-based line of the `let`.
    pub line: usize,
    /// What happens to the value, in one line — [`crate::own::Fate::words`].
    pub text: String,
    /// The line where the value stops being live, when there is one: a move or
    /// a `drop`. `None` for a binding that lives to block exit.
    pub last_use: Option<usize>,
    /// What took it, for the inlay hint. `Some` exactly when `last_use` is a
    /// move.
    pub moved_into: Option<String>,
}

/// One `import * as ns` binding and the exported declarations it exposes
/// (RFC-0027). Each member carries its source file + line for cross-file
/// go-to-definition, exactly like an ordinary imported [`Symbol`].
#[derive(Debug, Clone)]
pub struct NamespaceInfo {
    pub name: String,
    pub members: Vec<Symbol>,
}

/// Answer to "what is at this cursor": the declaration it resolves to.
#[derive(Debug, Clone)]
pub struct Resolution {
    pub name: String,
    pub kind: SymbolKind,
    /// Declaration location (for go-to-definition).
    pub target_line: usize,
    pub target_col: usize,
    pub target_end_col: usize,
    /// The file the declaration lives in — `None` for the open document,
    /// `Some(path)` for an imported symbol (cross-file go-to-definition).
    /// A remote module key (`github:...`) is not a jumpable path; the LSP
    /// returns "no definition" for those.
    pub target_file: Option<String>,
    /// Detail text (for hover).
    pub hover: String,
    /// Whether there is a real source declaration to jump to. `false` for a
    /// synthesized built-in method (e.g. `push`, `info`): it has hover text but
    /// no definition site, so go-to-definition returns nothing. Always `true`
    /// for user symbols and locals.
    pub definition: bool,
}

/// One completion item (a top-level symbol).
#[derive(Debug, Clone)]
pub struct Completion {
    pub label: String,
    pub kind: SymbolKind,
    pub detail: String,
    /// RFC-0051 §1: the item's `///` documentation, when its declaration has one.
    pub doc: Option<String>,
}

/// Lex, parse, type-check, move-check, and index `source` in one pass.
///
/// On a lex error, `symbols`/`tokens`/`locals` are empty and `diagnostics`
/// carries the single lex error (the lexer stops at the first illegal token,
/// leaving nothing to index).
///
/// On a parse error, the parser **recovers** (RFC-0006) both between top-level
/// declarations AND between statements inside a body, so `diagnostics` may carry
/// several parse errors while the recovered (partial) program is STILL indexed:
/// `symbols`/`tokens`/`locals` are populated, so hover, outline, and completion
/// keep working as you type. Downstream type/ownership checks are SKIPPED
/// whenever any parse error exists (a partial AST would only cascade), so with
/// parse errors present `diagnostics` holds parse errors only.
pub fn analyze(source: &str) -> Analysis {
    analyze_inner(source, None)
}

/// Like [`analyze`], but resolves the document's `import`s through the module
/// loader (RFC-0010), so multi-file programs get real diagnostics in the
/// editor instead of "unknown name" noise for every imported binding.
///
/// Symbols/locals/completions still index the ROOT document only (hover and
/// go-to-definition across files is future work); diagnostics come from the
/// fully linked program. A problem inside an imported module is reported at
/// line 0 with an `in <file>: ...` prefix, so it is visible without being
/// mis-anchored in the open document.
pub fn analyze_linked(
    source: &str,
    root_path: &str,
    opts: &crate::loader::LoadOptions,
    resolver: &dyn crate::loader::ModuleResolver,
) -> Analysis {
    analyze_inner(source, Some((root_path, opts, resolver)))
}

/// Rewrite a foreign-file diagnostic so it is visible (but not mis-anchored)
/// in the root document.
fn adopt_foreign(mut d: Diagnostic) -> Diagnostic {
    if let Some(file) = d.file.take() {
        d.message = format!("in {file}: {}", d.message);
        d.line = 0;
        d.col = 0;
        d.end_col = 0;
    }
    d
}

fn analyze_inner(
    source: &str,
    linker: Option<(
        &str,
        &crate::loader::LoadOptions,
        &dyn crate::loader::ModuleResolver,
    )>,
) -> Analysis {
    let tokens = match lexer::lex(source) {
        Ok(t) => t,
        Err(d) => return empty_analysis(vec![d]),
    };
    // Cache identifier tokens (for cursor → token mapping and for finding each
    // declaration name's column — the AST only keeps the line) PLUS `.` tokens.
    // The dots are what lets [`member_completions`] find the receiver identifier
    // immediately before a `.foo` access; no declaration or local is ever named
    // `.`, so the dot entries are inert for [`resolve`] / name-column searches.
    let tok_info: Vec<TokenInfo> = tokens
        .iter()
        .filter_map(|t| match &t.tok {
            Tok::Ident(s) => Some(TokenInfo {
                text: s.clone(),
                line: t.line,
                col: t.col,
                end_col: t.col + s.chars().count(),
            }),
            Tok::Dot => Some(TokenInfo {
                text: ".".to_string(),
                line: t.line,
                col: t.col,
                end_col: t.col + 1,
            }),
            _ => None,
        })
        .collect();
    // First column of each keyword/operator token, per line. Captured before
    // `tokens` is moved into the parser, so line-only checker/movecheck
    // diagnostics can be pinned to the precise keyword/operator they're about
    // (e.g. an `if` whose condition isn't Bool, a `where` clause on a record)
    // rather than the whole line. See [`pin_diagnostics`].
    let mut kw_cols: std::collections::HashMap<
        usize,
        std::collections::HashMap<&'static str, (usize, usize)>,
    > = std::collections::HashMap::new();
    for t in tokens.iter() {
        if let Some(text) = keyword_text(&t.tok) {
            kw_cols
                .entry(t.line)
                .or_default()
                .entry(text)
                .or_insert((t.col, t.col + text.len()));
        }
    }

    let (program, parse_errors) = parser::parse_accum(tokens);
    // Statement-level recovery (RFC-0006) means a body parse error no longer
    // discards the program: `program` is still a usable (partial) AST, so the
    // symbol/token/local index below keeps hover, outline, and completion alive
    // while the user is mid-edit. But downstream type/ownership checks are
    // SKIPPED whenever ANY parse error exists — running the checker on a
    // recovered partial AST would only cascade into bogus "unknown"/mismatch
    // diagnostics on top of the real syntax error.
    let parse_failed = !parse_errors.is_empty();
    let mut diags: Vec<Diagnostic> = parse_errors;

    // With a linker and any imports, check the fully LINKED program; the
    // parsed root keeps powering the symbol index below. `None` = checks
    // skipped (parse failed) or linking failed (load diagnostics already in
    // `diags`).
    // RFC-0033 origin maps for the linked program, plus the diagnostics that
    // remap into a generator input file (published against that file's URI).
    let mut origins = crate::origin::OriginMaps::default();
    let mut graph: crate::loader::ModuleGraph = Vec::new();
    let mut remapped: Vec<Diagnostic> = Vec::new();
    let checked: Option<crate::ast::Program> = if parse_failed {
        None
    } else {
        match (&linker, program.imports.is_empty()) {
            (Some((root_path, opts, resolver)), false) => {
                let (loaded, o, load_warnings, g) =
                    crate::loader::load_with_origins(source, root_path, opts, *resolver);
                graph = g;
                // RFC-0053: the maps come back even when the load FAILED (they
                // need no successful parse), so a `.vyx` whose template stopped
                // lexing still knows its owner and still gets its squiggle.
                origins = o;
                // RFC-0071 M2b: a warning is already positioned at the input file
                // the generator named, so it routes exactly like a remapped error
                // — published against that file's URI, at the line the user wrote.
                for d in load_warnings {
                    if d.from_generated {
                        remapped.push(d);
                    } else {
                        diags.push(adopt_foreign(d));
                    }
                }
                match loaded {
                    Ok(linked) => Some(linked),
                    Err(load_diags) => {
                        // RFC-0053: the loader already remapped lex/parse/load
                        // failures inside a synthesized module onto the `.vyx`
                        // (or other input) they came from. Those name a real
                        // file, so they join `remapped` and are published
                        // against that file's URI — the squiggle lands in the
                        // buffer the user is typing in. Everything else keeps
                        // the existing foreign-adoption behavior.
                        for d in load_diags {
                            if d.from_generated {
                                remapped.push(d);
                            } else {
                                diags.push(adopt_foreign(d));
                            }
                        }
                        None
                    }
                }
            }
            _ => Some(program.clone()),
        }
    };
    // `check_accum_with_let_types` returns the diagnostics AND a table of the
    // inferred/declared type of each clean `let` and `for`-var — used below to
    // give unannotated lets a real type on hover (`let x: Int`).
    let let_types = match &checked {
        Some(prog) => {
            // The LSP re-checks on every keystroke, and all but the edited
            // module are byte-identical to last time — reuse their diagnostics
            // when no signature moved. Effects are not needed here (the workers
            // gate uses the full check), which is why the reusing entry does not
            // return them.
            let hashes = crate::loader::last_module_hashes();
            let (check_diags, let_types) = if hashes.is_empty() {
                checker::check_accum_with_let_types(prog)
            } else {
                checker::check_accum_reusing(prog, &hashes)
            };
            let mut checked_diags = check_diags;
            checked_diags.extend(movecheck::check_accum(prog));
            // RFC-0033: a diagnostic at an origin-governed line in a synthesized
            // module is relocated to its input file (`.vyx`, …) and set aside so
            // the LSP can publish it against that file's URI; everything else
            // keeps the existing foreign-adoption behavior (shown in the root at
            // line 0). A no-op when no generator emitted directives.
            for mut d in checked_diags {
                if !origins.is_empty() && origins.remap(&mut d) {
                    remapped.push(d);
                } else {
                    diags.push(adopt_foreign(d));
                }
            }
            let_types
        }
        None => Default::default(),
    };
    pin_diagnostics(&mut diags, &kw_cols, &tok_info);

    // RFC-0073 M3: the symbol map every generated module bakes in, so a symbol
    // that stands for a declaration resolves to the DECLARATION — its file, its
    // line, its doc comment, and its derived wire facts. Empty when nothing was
    // generated, and cheap when something was: a module with no map is a failed
    // substring search over its source.
    let origin_index = match linker {
        Some((_, _, resolver)) if !graph.is_empty() => OriginIndex::build(&graph, resolver),
        _ => OriginIndex::default(),
    };

    let decl_lines = decl_lines(&program);
    let fn_lines = fn_lines(&program);
    let mut symbols = index_symbols(&program, &tok_info, &decl_lines);
    // Cross-file symbols: declarations the root imports, indexed from the
    // linked program with their source file, so hover shows the signature and
    // go-to-definition jumps into the imported module. A no-op without a
    // linker (a plain-`analyze` program has no `module`-tagged decls).
    if let Some(linked) = &checked {
        symbols.extend(index_imported_symbols(&program, linked, &origin_index));
    }
    // RFC-0027: namespace bindings and their reachable exports (for `ns.`
    // completion and `ns.member` hover / go-to-definition). Needs the linker to
    // resolve each namespace import to its source module.
    let namespaces = index_namespaces(&graph, &program, linker, &origin_index);
    let locals = index_locals(&program, &tok_info, &let_types);

    // Protocol/impl member tables for `.foo` completion (RFC-0002 §5). Impls
    // and protocols come from the linked program when available (imported
    // impls count — coherence makes them global); bounds come from the root's
    // functions (only root bodies have a cursor).
    let member_src = checked.as_ref().unwrap_or(&program);
    let mut impl_members = Vec::new();
    for imp in &member_src.impls {
        for m in &imp.methods {
            let doc = m
                .doc
                .clone()
                .or_else(|| signature_doc(&member_src.protocols, &imp.protocol, &m.name));
            impl_members.push((
                imp.ty.clone(),
                Completion {
                    label: m.name.clone(),
                    kind: SymbolKind::Method,
                    detail: function_detail(m),
                    doc,
                },
            ));
        }
    }
    let mut protocol_members = Vec::new();
    for p in &member_src.protocols {
        for m in &p.methods {
            protocol_members.push((
                p.name.clone(),
                Completion {
                    label: m.name.clone(),
                    kind: SymbolKind::Method,
                    detail: method_sig_detail(m),
                    doc: m.doc.clone(),
                },
            ));
        }
    }
    let type_param_bounds = program
        .functions
        .iter()
        .chain(program.impls.iter().flat_map(|i| i.methods.iter()))
        .flat_map(|f| {
            f.type_bounds
                .iter()
                .map(|(tp, bs)| (f.line, tp.clone(), bs.clone()))
        })
        .collect();
    let mut record_fields = Vec::new();
    for t in &member_src.type_decls {
        // Skip synthetic inline-refinement decls (`User.age`) — their parent
        // record is the one whose fields matter.
        if t.name.contains('.') {
            continue;
        }
        if let Type::Record(fields) = &t.base {
            for f in fields {
                record_fields.push((
                    t.name.clone(),
                    Completion {
                        label: f.name.clone(),
                        kind: SymbolKind::Field,
                        detail: field_detail(f, &member_src.type_decls),
                        doc: None,
                    },
                ));
            }
        }
    }

    // RFC-0020 M1: enumerate each finite string type's language once (≤ cap), and
    // record top-level fn parameter types, for string-literal completion.
    const STRING_COMPLETION_CAP: usize = 1000;
    let mut finite_string_types = Vec::new();
    for t in &member_src.type_decls {
        if t.name.contains('.') {
            continue;
        }
        if let Some(domain) = crate::finite::enumerate_type(t, STRING_COMPLETION_CAP) {
            finite_string_types.push((t.name.clone(), domain));
        }
    }
    let fn_param_types = member_src
        .functions
        .iter()
        .map(|f| {
            (
                f.name.clone(),
                f.params.iter().map(|p| p.ty.clone()).collect(),
            )
        })
        .collect();

    // RFC-0042: enumerate the token alphabet of each *sequence* validated string
    // type (an infinite space-separated regex over a finite alphabet — a `Tw`),
    // for class-attribute token-in-sequence completion. A higher cap than the
    // finite whole-domain path: a realistic theme's alphabet (utilities × bounded
    // prefixes + safelist) runs to a couple thousand tokens.
    const CLASS_ALPHABET_CAP: usize = 8192;
    let mut sequence_string_types = Vec::new();
    for t in &member_src.type_decls {
        if t.name.contains('.') {
            continue;
        }
        if let Some(alphabet) = crate::finite::enumerate_alphabet(t, CLASS_ALPHABET_CAP) {
            sequence_string_types.push((t.name.clone(), alphabet));
        }
    }
    // Capture the theme stylesheet (`std/tw` `css()` — a baked constant) only when a
    // sequence type is present, so hover on a class token can show its CSS rule.
    // Gating on a present `Tw` avoids picking up an unrelated user `css()`.
    let tw_css = if sequence_string_types.is_empty() {
        String::new()
    } else {
        css_constant(member_src).unwrap_or_default()
    };

    // RFC-0087 U1. Only when the checks ran and found nothing: `own::analyze`
    // reads a program the checker approved, and an answer about a body that does
    // not compile is an answer about a body nobody will run. This is also what
    // keeps it off the hot path — an editor spends most of a keystroke burst on
    // a document that does not parse.
    let memory = match &checked {
        Some(prog) if !diags.iter().any(|d| d.severity == crate::diagnostics::Severity::Error) => memory_notes(prog),
        _ => Vec::new(),
    };

    Analysis {
        diagnostics: diags,
        memory,
        symbols,
        tokens: tok_info,
        locals,
        decl_lines,
        fn_lines,
        impl_members,
        protocol_members,
        type_param_bounds,
        record_fields,
        finite_string_types,
        fn_param_types,
        sequence_string_types,
        tw_css,
        namespaces,
        origins,
        remapped,
        symbol_maps: origin_index.all,
    }
}

/// Every `let` in the ROOT module, with what the ownership analysis decided
/// (RFC-0087 U1).
///
/// A linked program carries every import's functions and they are another file's
/// answer, so the filter is the same one `vyrn why --memory` uses: a function
/// with no `module` tag is this document's.
fn memory_notes(program: &crate::ast::Program) -> Vec<MemoryNote> {
    let own = crate::own::analyze(program);
    let mut out = Vec::new();
    for f in program.functions.iter().filter(|f| f.module.is_none() && !f.is_extern) {
        let Some(notes) = own.notes.get(&f.name) else { continue };
        for n in notes {
            // A binding whose type owns no heap has nothing to reclaim, so
            // "NOT reclaimed" is the wrong sentence about it. `vyrn why --memory`
            // counts it in a summary; a hover on an `Int64` would just alarm.
            if matches!(&n.fate, crate::own::Fate::Leaked(crate::own::Leak::NoRelease(_))) {
                continue;
            }
            out.push(MemoryNote {
                name: n.name.clone(),
                line: n.line,
                text: n.fate.words(),
                last_use: n.fate.last_use(),
                moved_into: match &n.fate {
                    crate::own::Fate::Moved { into, .. } => Some(into.clone()),
                    _ => None,
                },
            });
        }
    }
    out
}

/// An `Analysis` with everything but `diagnostics` empty (lex/parse failure).
fn empty_analysis(diagnostics: Vec<Diagnostic>) -> Analysis {
    Analysis {
        diagnostics,
        symbols: Vec::new(),
        tokens: Vec::new(),
        locals: Vec::new(),
        decl_lines: Vec::new(),
        fn_lines: Vec::new(),
        impl_members: Vec::new(),
        protocol_members: Vec::new(),
        type_param_bounds: Vec::new(),
        record_fields: Vec::new(),
        finite_string_types: Vec::new(),
        fn_param_types: Vec::new(),
        sequence_string_types: Vec::new(),
        tw_css: String::new(),
        namespaces: Vec::new(),
        origins: crate::origin::OriginMaps::default(),
        remapped: Vec::new(),
        symbol_maps: Vec::new(),
        memory: Vec::new(),
    }
}

/// The source text of a keyword/operator `Tok`, or `None` for identifiers,
/// literals, and punctuation not used in any error message. Used to build the
/// per-line keyword-column map consumed by [`pin_diagnostics`].
fn keyword_text(t: &Tok) -> Option<&'static str> {
    match t {
        Tok::Fn => Some("fn"),
        Tok::Let => Some("let"),
        Tok::Mut => Some("mut"),
        Tok::If => Some("if"),
        Tok::Else => Some("else"),
        Tok::While => Some("while"),
        Tok::For => Some("for"),
        Tok::In => Some("in"),
        Tok::Drop => Some("drop"),
        Tok::Protocol => Some("protocol"),
        Tok::Impl => Some("impl"),
        Tok::Vself => Some("self"),
        Tok::Return => Some("return"),
        Tok::True => Some("true"),
        Tok::False => Some("false"),
        Tok::Type => Some("type"),
        Tok::Where => Some("where"),
        Tok::Match => Some("match"),
        Tok::Region => Some("region"),
        Tok::Spawn => Some("spawn"),
        Tok::Question => Some("?"),
        Tok::AndAnd => Some("&&"),
        Tok::OrOr => Some("||"),
        Tok::Bang => Some("!"),
        _ => None,
    }
}

/// The text inside each backtick-quoted span of `msg`, in order — e.g.
/// `` `match` is missing variant `B` `` → `["match", "B"]`. These name the
/// keyword or identifier a diagnostic is about; the first one that occurs on
/// the diagnostic's line is the pin target.
fn backtick_tokens(msg: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = msg;
    while let Some(open) = rest.find('`') {
        rest = &rest[open + 1..];
        if let Some(close) = rest.find('`') {
            out.push(&rest[..close]);
            rest = &rest[close + 1..];
        } else {
            break;
        }
    }
    out
}

/// Pin line-only checker/movecheck diagnostics to the precise token they're
/// about, instead of leaving them at `col == 0` (which the LSP would squiggle
/// as the whole line — leading spaces and unrelated tokens like `return`
/// included).
///
/// The checker and movecheck internals report errors knowing only the line:
/// they emit `"line {N}: ..."` strings lifted by `Diagnostic::from_rendered`
/// with `col == 0`. Nearly every such message backtick-quotes the offending
/// keyword or name (`` `if` condition must be Bool ``, `` unknown variable
/// `x` ``, `` `{x}` is used here but was already consumed ``). For each
/// quoted token, this looks for its column on the error's line — first among
/// identifier tokens (user names: variables, types, variants, built-in call
/// names like `len`/`Merge`/`Partial`, movecheck consumed vars), then among
/// keyword/operator tokens — and writes it into the diagnostic. Reserved words
/// are never identifiers, so an `if`/`while`/`where`/`drop`/`match` target
/// always resolves via the keyword map. If no target is found on the line the
/// diagnostic stays line-only: a graceful whole-line fallback.
///
/// This is positions-only: it touches no message text and doesn't change
/// `render()` (so the `check()` shim and the message half of `vyrn check`
/// output are unchanged). The `:col:` prefix in `vyrn check` becomes precise
/// for pinned diagnostics, as it already was for `match`.
fn pin_diagnostics(
    diags: &mut [Diagnostic],
    kw_cols: &std::collections::HashMap<
        usize,
        std::collections::HashMap<&'static str, (usize, usize)>,
    >,
    tok_info: &[TokenInfo],
) {
    for d in diags.iter_mut() {
        // Already-precise diagnostics (lex/parse carry their own column) are
        // left alone; only line-only ones (`col == 0`) are candidates.
        if d.col != 0 {
            continue;
        }
        for target in backtick_tokens(&d.message) {
            // Identifier path: a user name on this line (the common case — the
            // offending use is on the error's line).
            if let Some(t) = tok_info
                .iter()
                .find(|t| t.line == d.line && t.text == target)
            {
                d.col = t.col;
                d.end_col = t.end_col;
                break;
            }
            // Keyword/operator path: `if`/`while`/`where`/`drop`/`match`/`?`/
            // `&&`/`||`/`!`. Reserved words are never identifiers, so the
            // identifier path above won't have matched them.
            if let Some(&(col, end_col)) = kw_cols.get(&d.line).and_then(|kws| kws.get(target)) {
                d.col = col;
                d.end_col = end_col;
                break;
            }
        }
        // No backtick target found on the line → stays `col == 0` (whole-line
        // fallback in the LSP), unchanged.
    }
}

/// Resolve a 1-based `(line, col)` cursor to the declaration it names.
///
/// A local binding (param / `let` / `for`-var) in the cursor's enclosing
/// function wins over a top-level symbol of the same name — that is the usual
/// shadowing a reader expects. Among same-named locals in scope, the latest
/// definition at or before the cursor wins (a binding is visible only from its
/// line onward; params are visible from the function's line). If no local
/// matches, this falls back to the top-level symbol index.
///
/// Scope is line-based, not block-based: a `let` inside an `if` is treated as
/// visible to the end of the function. This is an over-approximation that only
/// matters when a binding's name is reused after its block ends — acceptable for
/// hover/go-to-def (the "latest preceding binding" heuristic is right in the
/// common case) and avoids threading real block scope through the AST.
///
/// If no user symbol or local matches, a built-in method/function name (`push`,
/// `info`, `len`, ...) resolves to a synthesized [`Resolution`] with hover text
/// but `definition: false` — there is no source declaration to jump to, so
/// go-to-definition returns nothing for it.
pub fn resolve(analysis: &Analysis, line: usize, col: usize) -> Option<Resolution> {
    // The identifier token covering the cursor (col is within [col, end_col)).
    let tok = analysis
        .tokens
        .iter()
        .find(|t| t.line == line && col >= t.col && col < t.end_col)?;

    // Local bindings first (they shadow top-level names). Only those in the
    // cursor's enclosing function, defined at or before the cursor line.
    if let Some(fn_line) = enclosing_fn_line(analysis, line) {
        let local = analysis
            .locals
            .iter()
            .filter(|b| b.fn_line == fn_line && b.name == tok.text && b.line <= line)
            .max_by_key(|b| b.line);
        if let Some(b) = local {
            return Some(local_resolution(analysis, b));
        }
    }

    // RFC-0027: `ns.member` — the member token, preceded by an in-scope (un-
    // shadowed) namespace, resolves to that module's export (hover + cross-file
    // go-to-definition). Checked before top-level symbols so a same-named local
    // decl doesn't capture a qualified member reference.
    if let Some(recv) = receiver_before_dot(analysis, tok.line, tok.col) {
        let shadowed = enclosing_fn_line(analysis, line).is_some_and(|fl| {
            analysis
                .locals
                .iter()
                .any(|b| b.fn_line == fl && b.name == recv && b.line <= line)
        });
        if !shadowed {
            if let Some(nsi) = analysis.namespaces.iter().find(|n| n.name == recv) {
                if let Some(m) = nsi.members.iter().find(|m| m.name == tok.text) {
                    return Some(Resolution {
                        name: m.name.clone(),
                        kind: m.kind,
                        target_line: m.line,
                        target_col: m.col,
                        target_end_col: m.end_col,
                        target_file: m.file.clone(),
                        hover: format!(
                            "{}\n\n— via namespace `{}`",
                            with_doc(&m.detail, &m.doc),
                            recv
                        ),
                        definition: m.file.is_some(),
                    });
                }
            }
        }
    }

    // RFC-0051 §2: `receiver.member` — a member token whose receiver has a known
    // type resolves to that member (a record field, a builtin/array/map member,
    // a protocol/impl method). Answered from the very table `.`-completion uses,
    // so hover and completion can never disagree. Checked before the top-level
    // fallback so `p.title` is the FIELD, not a same-named top-level decl.
    if is_member_position(analysis, tok) {
        if let Some(c) = member_completions(analysis, tok.line, tok.col)
            .into_iter()
            .find(|c| c.label == tok.text)
        {
            // A user protocol/impl method IS an indexed symbol — keep its
            // declaration site so go-to-definition still works on `x.show()`.
            // A record field or builtin method has none: it hovers, no jump.
            let decl = analysis
                .symbols
                .iter()
                .filter(|s| s.name == c.label && s.kind == SymbolKind::Method)
                .max_by_key(|s| (s.file.is_none(), s.line));
            return Some(Resolution {
                name: c.label.clone(),
                kind: c.kind,
                target_line: decl.map_or(0, |d| d.line),
                target_col: decl.map_or(0, |d| d.col),
                target_end_col: decl.map_or(0, |d| d.end_col),
                target_file: decl.and_then(|d| d.file.clone()),
                hover: with_doc(&c.detail, &c.doc.clone().or_else(|| decl.and_then(|d| d.doc.clone()))),
                definition: decl.is_some(),
            });
        }
    }

    // RFC-0027: hovering the namespace binding itself (`ns` in `import * as ns`
    // or in `ns.member`) — a compile-time name, not a value.
    if let Some(nsi) = analysis.namespaces.iter().find(|n| n.name == tok.text) {
        return Some(Resolution {
            name: nsi.name.clone(),
            kind: SymbolKind::Type,
            target_line: 0,
            target_col: 0,
            target_end_col: 0,
            target_file: None,
            hover: format!(
                "namespace `{}` — {} exported member(s) (a compile-time name, not a value)",
                nsi.name,
                nsi.members.len()
            ),
            definition: false,
        });
    }

    // Fall back to top-level symbols. A symbol of the open document (`file:
    // None`) wins over an imported one of the same name (module scoping: the
    // local declaration shadows); among candidates, the latest declaration
    // wins (max_by_key returns the last on ties).
    let best = analysis
        .symbols
        .iter()
        .filter(|s| s.name == tok.text)
        .max_by_key(|s| (s.file.is_none(), s.line));
    if let Some(best) = best {
        return Some(Resolution {
            name: best.name.clone(),
            kind: best.kind,
            target_line: best.line,
            target_col: best.col,
            target_end_col: best.end_col,
            target_file: best.file.clone(),
            hover: with_doc(&best.detail, &best.doc),
            definition: true,
        });
    }

    // A built-in method/function name (`push`, `info`, `len`, ...). These have
    // hover text but no source declaration, so go-to-definition has nowhere to
    // jump (`definition: false`).
    if let Some(b) = builtin_method(&tok.text) {
        return Some(Resolution {
            name: b.name.to_string(),
            kind: SymbolKind::Method,
            target_line: 0,
            target_col: 0,
            target_end_col: 0,
            target_file: None,
            hover: b.detail.to_string(),
            definition: false,
        });
    }

    // Last resort: the ambient `Result`/`Option` builtins and their constructors
    // (RFC-0062). Whether or not they were spelled as explicit `std/result` /
    // `std/option` imports, hovering one shows what it is — like any other
    // builtin, it has no source declaration to jump to.
    builtin_type_or_ctor(&tok.text).map(|(kind, hover)| Resolution {
        name: tok.text.clone(),
        kind,
        target_line: 0,
        target_col: 0,
        target_end_col: 0,
        target_file: None,
        hover,
        definition: false,
    })
}

/// Hover text + symbol kind for the ambient `Result`/`Option` builtins and their
/// constructors — the names `std/result` / `std/option` spell explicitly
/// (RFC-0062). `None` for anything else.
fn builtin_type_or_ctor(name: &str) -> Option<(SymbolKind, String)> {
    let (kind, detail) = match name {
        "Result" => (SymbolKind::Type, "Result<T, E> — the builtin result type (`Ok(T)` | `Err(E)`). Spelled explicitly by `import { Result, Ok, Err } from \"std/result\"`."),
        "Ok" => (SymbolKind::Variant, "Ok(value: T) -> Result<T, E> — the success variant of the builtin `Result`."),
        "Err" => (SymbolKind::Variant, "Err(error: E) -> Result<T, E> — the failure variant of the builtin `Result`."),
        "Option" => (SymbolKind::Type, "Option<T> — the builtin option type (`Some(T)` | `None`). Spelled explicitly by `import { Option, Some, None } from \"std/option\"`."),
        "Some" => (SymbolKind::Variant, "Some(value: T) -> Option<T> — the present variant of the builtin `Option`."),
        "None" => (SymbolKind::Variant, "None -> Option<T> — the absent variant of the builtin `Option`."),
        _ => return None,
    };
    Some((kind, detail.to_string()))
}

/// The function whose line range contains `cursor_line`, if any. A function's
/// range is `[fn_line, next_decl_line)` — bounded by the next top-level decl so
/// a cursor in a later type/protocol decl is not attributed to the preceding fn.
fn enclosing_fn_line(analysis: &Analysis, cursor_line: usize) -> Option<usize> {
    // The top-level decl segment the cursor falls in: the greatest decl line <=
    // cursor. (If the cursor is before the first decl, there is no segment.)
    let seg_start = analysis
        .decl_lines
        .iter()
        .rev()
        .find(|&&l| l <= cursor_line)
        .copied()?;
    // That segment is a function iff its decl line is a function line.
    analysis
        .fn_lines
        .iter()
        .any(|&l| l == seg_start)
        .then_some(seg_start)
}

/// Whether the 1-based `(line, col)` cursor sits at **module scope** in
/// `source` — outside every brace-delimited body.
///
/// RFC-0071 M4 needs this: a contract member is a declaration a module makes, so
/// `head`/`data` are offered where a declaration can go and nowhere else. The
/// answer is the brace depth of the token stream before the cursor, which is
/// exact (the lexer already knows a `{` inside a string is not a brace) and
/// costs one lex — no parse, no check, so it is affordable per keystroke and
/// works on a buffer that does not parse yet.
pub fn at_module_scope(source: &str, line: usize, col: usize) -> bool {
    let Ok(tokens) = lexer::lex(source) else {
        // A buffer that does not lex has no reliable structure; the caller's
        // fallback (offer nothing extra) is the safe answer.
        return false;
    };
    let mut depth: i64 = 0;
    for t in &tokens {
        if (t.line, t.col) >= (line, col) {
            break;
        }
        match t.tok {
            Tok::LBrace => depth += 1,
            Tok::RBrace => depth -= 1,
            _ => {}
        }
    }
    depth <= 0
}

/// All top-level symbols as completion items. The client filters by the prefix
/// the user typed; v1 does no scope-aware filtering.
pub fn completions(analysis: &Analysis) -> Vec<Completion> {
    let mut out: Vec<Completion> = analysis
        .symbols
        .iter()
        .map(|s| Completion {
            label: s.name.clone(),
            kind: s.kind,
            detail: s.detail.clone(),
            doc: s.doc.clone(),
        })
        .collect();
    // RFC-0062: the ambient `Result`/`Option` builtins and their constructors are
    // always in scope — offer them alongside user symbols (they are exactly what
    // `std/result` / `std/option` name explicitly), so `Ok`/`Some`/… complete.
    for name in ["Result", "Ok", "Err", "Option", "Some", "None"] {
        if let Some((kind, detail)) = builtin_type_or_ctor(name) {
            out.push(Completion { label: name.to_string(), kind, detail, doc: None });
        }
    }
    out
}

/// Context-aware completions for a `.foo` member access: given a cursor on (or
/// just after) a `.` on `line`, resolve the receiver's type — the identifier
/// immediately before the dot — and return the members applicable to that type:
/// built-in methods (`Array.push`, `Logger.info`, `Ref.get`, ...), the `length`
/// field on arrays, and the fields of a record receiver.
///
/// Returns an empty list when the receiver can't be typed: a non-identifier
/// receiver (a literal or a chained call — only simple-identifier receivers are
/// handled), a receiver whose binding isn't in the local index, or an unknown
/// type. The caller (the LSP) treats empty as "no member suggestions" and falls
/// back to top-level [`completions`] only when not in a `.foo` context at all.
///
/// User `protocol`/`impl` methods (RFC-0002 §5) are offered too: a concrete
/// receiver gets the methods of every `impl P for T` matching its type; a
/// bounded generic receiver (`fn f<T: Show>(x: T)` → `x.`) gets each bound
/// protocol's methods — mirroring the checker's static dispatch.
pub fn member_completions(analysis: &Analysis, line: usize, col: usize) -> Vec<Completion> {
    // RFC-0027: if the receiver names an in-scope namespace (and isn't shadowed
    // by a local), offer that module's exported members after the dot.
    if let Some(recv) = receiver_before_dot(analysis, line, col) {
        let shadowed = enclosing_fn_line(analysis, line).is_some_and(|fl| {
            analysis
                .locals
                .iter()
                .any(|b| b.fn_line == fl && b.name == recv && b.line <= line)
        });
        if !shadowed {
            if let Some(nsi) = analysis.namespaces.iter().find(|n| n.name == recv) {
                return nsi
                    .members
                    .iter()
                    .map(|s| Completion {
                        label: s.name.clone(),
                        kind: s.kind,
                        detail: format!("{}\n\n— via namespace `{}`", s.detail, recv),
                        doc: s.doc.clone(),
                    })
                    .collect();
            }
        }
    }
    // Resolve the receiver's type (see `resolve_receiver_type`); if it can't be
    // typed, there is nothing to suggest after the dot.
    let ty = match resolve_receiver_type(analysis, line, col) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut out: Vec<Completion> = builtin_methods_for(&ty)
        .iter()
        .map(|b| Completion {
            label: b.name.to_string(),
            kind: SymbolKind::Method,
            detail: b.detail.to_string(),
            doc: None,
        })
        .collect();
    // `arr.length` is the element-count field sugar — a read-only field on
    // arrays, surfaced as a member alongside the array methods.
    if matches!(ty, Type::Array(_) | Type::ArrayN(..) | Type::SmallArray(..)) {
        out.push(Completion {
            label: "length".to_string(),
            kind: SymbolKind::Field,
            detail: "length: Int64 — element count (read-only)".to_string(),
            doc: None,
        });
    }
    // `map.length` is the entry-count field sugar (RFC-0028).
    if matches!(ty, Type::Map(..)) {
        out.push(Completion {
            label: "length".to_string(),
            kind: SymbolKind::Field,
            detail: "length: Int64 — entry count (read-only)".to_string(),
            doc: None,
        });
    }
    // `str.byteLength` is the UTF-8 byte count field (RFC-0058); `.charCount()`
    // (a method) answers the Unicode-scalar question. `String` has no `.length`.
    if matches!(ty, Type::Str) {
        out.push(Completion {
            label: "byteLength".to_string(),
            kind: SymbolKind::Field,
            detail: "byteLength: Int64 — UTF-8 byte count (O(1), read-only)".to_string(),
            doc: None,
        });
    }
    // Record fields: a named record receiver offers its declaration's fields;
    // an inline structural receiver offers its own.
    match &ty {
        Type::Named(n) => {
            for (tn, c) in &analysis.record_fields {
                if tn == n {
                    out.push(c.clone());
                }
            }
        }
        Type::Record(fields) => {
            for f in fields {
                out.push(Completion {
                    label: f.name.clone(),
                    kind: SymbolKind::Field,
                    detail: format!("{}: {}", f.name, type_to_string(&f.ty)),
                    doc: None,
                });
            }
        }
        _ => {}
    }
    // User protocol methods (RFC-0002 §5). Concrete receiver: every
    // `impl P for T` whose T equals the receiver's type contributes its
    // methods (`n: Int64` + `impl Show for Int64` → `n.show`).
    for (t, c) in &analysis.impl_members {
        if *t == ty {
            out.push(c.clone());
        }
    }
    // Bounded generic receiver: `x: T` inside `fn f<T: Show>` offers `Show`'s
    // method signatures — exactly what the checker's static dispatch permits.
    // (A type-parameter reference parses as `Type::Param`; `Named` is matched
    // too, defensively, since the two render identically.)
    if let Type::Named(n) | Type::Param(n) = &ty {
        if let Some(fn_line) = enclosing_fn_line(analysis, line) {
            for (fl, tp, bounds) in &analysis.type_param_bounds {
                if *fl == fn_line && tp == n {
                    for b in bounds {
                        for (pn, c) in &analysis.protocol_members {
                            if pn == b {
                                out.push(c.clone());
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

/// RFC-0020 M1: completions for the *content* of a string literal whose expected
/// type is a finite validated string type — `t("` offers every key. Returns the
/// enumerated language of the expected type (up to the analysis cap), or an empty
/// list when the cursor is not inside a string whose expected type is a known
/// finite string type (an over-cap or infinite type has no entry, so it too
/// yields nothing).
///
/// v1 expected-type detection (from the token stream): the string literal is
/// either
/// - a direct argument of a call `f(… "…" …)` to a known top-level function,
///   mapped to that parameter's type by comma position, or
/// - the initializer of an annotated `let name: T = "…"`.
pub fn string_literal_completions(
    analysis: &Analysis,
    source: &str,
    line: usize,
    col: usize,
) -> Vec<Completion> {
    let Some(ty_name) = expected_string_type(analysis, source, line, col) else {
        return Vec::new();
    };
    let Some((_, domain)) = analysis
        .finite_string_types
        .iter()
        .find(|(n, _)| n == &ty_name)
    else {
        return Vec::new();
    };
    domain
        .iter()
        .map(|k| Completion {
            label: k.clone(),
            kind: SymbolKind::Variant,
            detail: format!("{ty_name} — a key of this finite string type"),
            doc: None,
        })
        .collect()
}

/// RFC-0042: completions for a `class="…"` / `theme.cls("…")` string whose
/// expected type is a **sequence** validated string type (a `Tw`). Returns the
/// theme's whole token alphabet (utilities + safelist), each labelled with a hint
/// detail; the caller filters by the whitespace-delimited token under the cursor
/// and applies the insertion (token-in-sequence). `None` when the cursor's string
/// is not a sequence-typed argument — the caller then falls back to the finite
/// whole-domain path ([`string_literal_completions`]) for keys, etc.
pub fn class_completions(
    analysis: &Analysis,
    source: &str,
    line: usize,
    col: usize,
) -> Option<Vec<Completion>> {
    // The string must be an argument of the std/tw checked bridge `cls(…)`
    // (`theme.cls("…")` in `.vyrn`, `vyxTheme.cls("…")` in a themed `.vyx`'s
    // generated code), with a sequence (`Tw`) string type in scope. Gating on the
    // `cls` call — the stable public bridge — rather than the parameter's declared
    // type, since a linked module lowers the `Tw` alias to its `String` base.
    if !cls_call_arg(source, line, col) {
        return None;
    }
    let (ty_name, alphabet) = analysis.sequence_string_types.first()?;
    Some(
        alphabet
            .iter()
            .map(|c| Completion {
                label: c.clone(),
                kind: SymbolKind::Variant,
                detail: format!("{ty_name} — theme utility/safelist class"),
                doc: None,
            })
            .collect(),
    )
}

/// Whether the string literal under the cursor is a direct argument of a `cls(…)`
/// call — the std/tw checked-class bridge. Re-lexes `source` and scans left from
/// the string to the enclosing `(`, requiring the callee identifier to be `cls`
/// (bare or the trailing segment of a namespaced `ns.cls`).
fn cls_call_arg(source: &str, line: usize, col: usize) -> bool {
    let Ok(toks) = lexer::lex(source) else {
        return false;
    };
    let Some(str_idx) = toks.iter().position(|t| {
        if t.line != line {
            return false;
        }
        if let Tok::Str(s) = &t.tok {
            let start = t.col;
            let end = t.col + s.chars().count() + 2;
            col >= start && col <= end
        } else {
            false
        }
    }) else {
        return false;
    };
    let mut depth = 0i32;
    let mut i = str_idx;
    while i > 0 {
        i -= 1;
        match &toks[i].tok {
            Tok::RParen => depth += 1,
            Tok::LParen => {
                if depth == 0 {
                    return i >= 1 && matches!(&toks[i - 1].tok, Tok::Ident(c) if c == "cls");
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    false
}

/// RFC-0042: hover text for the class token under the cursor inside a
/// sequence-typed (`Tw`) `class="…"` / `theme.cls("…")` string. Shows the exact
/// CSS rule `std/tw` `css()` emits for a utility, or "safelisted (app-styled)" for
/// a safelist entry (no rule). `None` when the cursor is not on a valid class
/// token of a linked theme.
pub fn class_token_hover(
    analysis: &Analysis,
    source: &str,
    line: usize,
    col: usize,
) -> Option<String> {
    if !cls_call_arg(source, line, col) {
        return None;
    }
    let (ty_name, alphabet) = analysis.sequence_string_types.first()?;
    let token = class_token_at(source, line, col)?;
    if !alphabet.iter().any(|c| c == &token) {
        return None;
    }
    match css_rule_for(&analysis.tw_css, &token) {
        Some(rule) => Some(format!("**`{token}`** — `{ty_name}` utility class\n\n```css\n{rule}\n```")),
        None => Some(format!("**`{token}`** — safelisted (app-styled)")),
    }
}

/// The whitespace-delimited token containing the cursor inside the string literal
/// on `line` — the class word the user is on/typing. 1-based `col`.
fn class_token_at(source: &str, line: usize, col: usize) -> Option<String> {
    let line_text = source.lines().nth(line.saturating_sub(1))?;
    let chars: Vec<char> = line_text.chars().collect();
    // 0-based index of the char the cursor sits just after (col is 1-based).
    let mut start = col.saturating_sub(1);
    if start > chars.len() {
        start = chars.len();
    }
    let is_word = |c: char| !c.is_whitespace() && c != '"' && c != '\'';
    // Walk left to the token start.
    let mut lo = start;
    while lo > 0 && is_word(chars[lo - 1]) {
        lo -= 1;
    }
    // Walk right to the token end.
    let mut hi = start;
    while hi < chars.len() && is_word(chars[hi]) {
        hi += 1;
    }
    if lo == hi {
        return None;
    }
    Some(chars[lo..hi].iter().collect())
}

/// Extract the CSS rule `css()` emits for a class name from the captured theme
/// stylesheet. `std/tw` renders base rules as `.<class> {…}` and variant rules
/// with the selector's `:` escaped (`.md\:hover\:bg-… {…}`), so the selector is
/// matched with `:` escaped. Returns the whole `.<selector> {…}` rule text, or
/// `None` when no rule exists (a safelisted name).
fn css_rule_for(css: &str, class: &str) -> Option<String> {
    if css.is_empty() {
        return None;
    }
    let escaped: String = class.chars().flat_map(|c| {
        if c == ':' { vec!['\\', ':'] } else { vec![c] }
    }).collect();
    // `css()` renders base rules as `.<class> {…}` and variant rules with the
    // selector's `:` escaped plus an appended pseudo (`.md\:hover\:bg-… :hover {…}`),
    // so match the escaped selector prefix and take the rule up to its `}`. The
    // leading `.` prevents a prefixed variant (`.md\:…:bg-brand-500`) from being
    // mistaken for the base `.bg-brand-500`.
    let selector = format!(".{escaped}");
    // Find an occurrence whose following char does not extend the class token
    // (so `.p-2` is not satisfied by `.p-20`, and a prefixed variant does not
    // masquerade as the base rule).
    let mut from = 0usize;
    while let Some(rel) = css[from..].find(&selector) {
        let at = from + rel;
        let after = css[at + selector.len()..].chars().next();
        if !matches!(after, Some(c) if c.is_ascii_alphanumeric() || c == '-') {
            let open = css[at..].find('{')?;
            let close = css[at + open..].find('}')?;
            return Some(css[at..at + open + close + 1].trim().to_string());
        }
        from = at + selector.len();
    }
    None
}

/// The constant string a zero-parameter `css()` function returns (`std/tw`'s baked
/// stylesheet), if present in the (linked) program. The body is a single
/// `return "<literal>"`, so the return expression is a string literal.
fn css_constant(program: &ast::Program) -> Option<String> {
    let f = program
        .functions
        .iter()
        .find(|f| f.name == "css" && f.params.is_empty())?;
    for stmt in &f.body.stmts {
        if let Stmt::Return { value: Some(Expr::Str(s)), .. } = stmt {
            return Some(s.clone());
        }
    }
    None
}

/// The name of the validated string type expected at the string literal under
/// the cursor, if that context is a call argument or an annotated `let`.
/// Re-lexes `source` (cheap) to work over positioned tokens including the string
/// literal, `(`, `,`, `:`, and `let` — which the cached identifier/dot token
/// index does not carry.
fn expected_string_type(
    analysis: &Analysis,
    source: &str,
    line: usize,
    col: usize,
) -> Option<String> {
    let toks = lexer::lex(source).ok()?;
    // Find the string-literal token whose span contains the cursor. The lexer
    // records the opening-quote column; the content spans the literal's rendered
    // length plus the two quotes (a lower bound that is exact for un-escaped
    // ASCII keys — the only place this matters).
    let str_idx = toks.iter().position(|t| {
        if t.line != line {
            return false;
        }
        if let Tok::Str(s) = &t.tok {
            let start = t.col;
            let end = t.col + s.chars().count() + 2; // + the two quotes
            col >= start && col <= end
        } else {
            false
        }
    })?;

    // Case A: annotated `let name : T = "…"`. The tokens immediately before the
    // string are `… : TypeIdent =`, so `toks[str_idx-3..str_idx]` is `: T =`.
    if str_idx >= 3 && toks[str_idx - 1].tok == Tok::Eq && toks[str_idx - 3].tok == Tok::Colon {
        if let Tok::Ident(tn) = &toks[str_idx - 2].tok {
            return Some(tn.clone());
        }
    }

    // Case B: a call argument `f( a0 , a1 , "…" , … )`. Scan left from the string
    // to the enclosing `(`, counting top-level commas for the argument index and
    // capturing the callee identifier just before the `(`.
    let mut depth = 0i32;
    let mut arg_index = 0usize;
    let mut i = str_idx;
    while i > 0 {
        i -= 1;
        match &toks[i].tok {
            Tok::RParen => depth += 1,
            Tok::LParen => {
                if depth == 0 {
                    // The callee is the identifier right before this `(`.
                    if i >= 1 {
                        if let Tok::Ident(callee) = &toks[i - 1].tok {
                            let params = analysis
                                .fn_param_types
                                .iter()
                                .find(|(n, _)| n == callee)
                                .map(|(_, p)| p)?;
                            if let Some(Type::Named(tn)) = params.get(arg_index) {
                                return Some(tn.clone());
                            }
                        }
                    }
                    return None;
                }
                depth -= 1;
            }
            Tok::Comma if depth == 0 => arg_index += 1,
            _ => {}
        }
    }
    None
}

/// Resolve the type of the receiver of a `.foo` access at `(line, col)` — the
/// identifier immediately before the nearest `.` at or before the cursor on
/// `line`. Returns `None` when there is no dot, no preceding identifier, the
/// identifier isn't a local in the enclosing function, or its type isn't known
/// (unannotated lets whose type the checker couldn't infer, or a top-level
/// receiver — only locals are method receivers in practice). Cloned so the
/// caller owns the type independent of the borrow on `analysis`.
/// The identifier text immediately before the dot at/preceding the cursor on
/// `line` — the receiver of a `.foo` member access. Shared by member-type
/// resolution (methods/fields) and namespace member completion (RFC-0027).
fn receiver_before_dot(analysis: &Analysis, line: usize, col: usize) -> Option<String> {
    let dot = analysis
        .tokens
        .iter()
        .filter(|t| t.line == line && t.text == "." && t.col <= col)
        .max_by_key(|t| t.col)?;
    let recv = analysis
        .tokens
        .iter()
        .filter(|t| t.line == line && t.text != "." && t.end_col <= dot.col)
        .max_by_key(|t| t.end_col)?;
    Some(recv.text.clone())
}

fn resolve_receiver_type(analysis: &Analysis, line: usize, col: usize) -> Option<Type> {
    // The identifier immediately before the dot on the same line — the receiver.
    let recv = receiver_before_dot(analysis, line, col)?;
    // Resolve the receiver's type from the local index (params/lets/for-vars —
    // the only things you call methods on in practice). Top-level receivers
    // (functions) aren't method receivers, so they're not handled.
    let fn_line = enclosing_fn_line(analysis, line)?;
    let binding = analysis
        .locals
        .iter()
        .filter(|b| b.fn_line == fn_line && b.name == recv && b.line <= line)
        .max_by_key(|b| b.line)?;
    binding.ty.clone()
}

// ---------------------------------------------------------------------------
// symbol indexing
// ---------------------------------------------------------------------------

/// Collect all top-level declaration lines (sorted) so a variant name search can
/// be bounded to its owning `type` declaration, and so a function's line range
/// can be bounded by the next declaration.
fn decl_lines(program: &ast::Program) -> Vec<usize> {
    let mut v: Vec<usize> = Vec::new();
    for f in &program.functions {
        v.push(f.line);
    }
    for t in &program.type_decls {
        v.push(t.line);
    }
    for p in &program.protocols {
        v.push(p.line);
    }
    for i in &program.impls {
        v.push(i.line);
    }
    for g in &program.globals {
        v.push(g.line);
    }
    v.sort_unstable();
    v
}

/// The sorted subset of [`decl_lines`] that are function declarations (functions
/// + impl methods). Protocol methods have no body, so they are not functions for
/// local-binding purposes.
fn fn_lines(program: &ast::Program) -> Vec<usize> {
    let mut v: Vec<usize> = Vec::new();
    for f in &program.functions {
        v.push(f.line);
    }
    for imp in &program.impls {
        for m in &imp.methods {
            v.push(m.line);
        }
    }
    v.sort_unstable();
    v
}

/// The first identifier matching `name` on `line` (a declaration with a known
/// AST line — the name is the first ident on that line).
fn name_col_on_line(tok_info: &[TokenInfo], name: &str, line: usize) -> (usize, usize) {
    tok_info
        .iter()
        .find(|t| t.text == name && t.line == line)
        .map(|t| (t.col, t.end_col))
        .unwrap_or((0, 0))
}

fn index_symbols(program: &ast::Program, tok_info: &[TokenInfo], lines: &[usize]) -> Vec<Symbol> {
    let mut out = Vec::new();

    for f in &program.functions {
        let (col, end_col) = name_col_on_line(tok_info, &f.name, f.line);
        out.push(Symbol {
            name: f.name.clone(),
            kind: SymbolKind::Function,
            line: f.line,
            col,
            end_col,
            detail: function_detail(f),
            doc: f.doc.clone(),
            file: None,
        });
    }

    // Module-state bindings (RFC-0013): top-level `let [mut] name [: Type] = ..`.
    for g in &program.globals {
        let (col, end_col) = name_col_on_line(tok_info, &g.name, g.line);
        out.push(Symbol {
            name: g.name.clone(),
            kind: SymbolKind::Global,
            line: g.line,
            col,
            end_col,
            detail: global_detail(g),
            doc: g.doc.clone(),
            file: None,
        });
    }

    for imp in &program.impls {
        for m in &imp.methods {
            let (col, end_col) = name_col_on_line(tok_info, &m.name, m.line);
            out.push(Symbol {
                name: m.name.clone(),
                kind: SymbolKind::Method,
                line: m.line,
                col,
                end_col,
                detail: function_detail(m),
                doc: m
                    .doc
                    .clone()
                    .or_else(|| signature_doc(&program.protocols, &imp.protocol, &m.name)),
                file: None,
            });
        }
    }

    for p in &program.protocols {
        let (col, end_col) = name_col_on_line(tok_info, &p.name, p.line);
        out.push(Symbol {
            name: p.name.clone(),
            kind: SymbolKind::Type,
            line: p.line,
            col,
            end_col,
            detail: protocol_detail(p),
            doc: p.doc.clone(),
            file: None,
        });
        for m in &p.methods {
            let (col, end_col) = name_col_on_line(tok_info, &m.name, m.line);
            out.push(Symbol {
                name: m.name.clone(),
                kind: SymbolKind::Method,
                line: m.line,
                col,
                end_col,
                detail: method_sig_detail(m),
                doc: m.doc.clone(),
                file: None,
            });
        }
    }

    for t in &program.type_decls {
        // Skip the parser-injected built-in `Value` enum (parser.rs injects it
        // with line == 0); it has no real source position. Synthetic inline
        // field-refinement types (`User.age` — the `.` is not a lexable
        // identifier) are desugaring artifacts, not user symbols.
        if t.line == 0 || t.name.contains('.') {
            continue;
        }
        let (col, end_col) = name_col_on_line(tok_info, &t.name, t.line);
        out.push(Symbol {
            name: t.name.clone(),
            kind: SymbolKind::Type,
            line: t.line,
            col,
            end_col,
            detail: type_decl_detail(t, &program.type_decls),
            doc: t.doc.clone(),
            file: None,
        });
        if let Type::Enum(variants) = &t.base {
            // Variants carry no AST line; find the name token between this decl's
            // line and the next top-level declaration (or EOF).
            let until = lines
                .iter()
                .find(|&&l| l > t.line)
                .copied()
                .unwrap_or(usize::MAX);
            for v in variants {
                let found = tok_info
                    .iter()
                    .find(|tt| tt.text == v.name && tt.line >= t.line && tt.line < until);
                let (col, end_col, vline) = match found {
                    Some(tt) => (tt.col, tt.end_col, tt.line),
                    None => (0, 0, t.line),
                };
                out.push(Symbol {
                    name: v.name.clone(),
                    kind: SymbolKind::Variant,
                    line: vline,
                    col,
                    end_col,
                    detail: variant_detail(&t.name, v),
                    doc: None,
                    file: None,
                });
            }
        }
    }

    // Tests (RFC-0015): show each `test "name"` block in the outline. The name is
    // a string literal (not an identifier token), so anchor the symbol at the
    // `test` keyword on the declaration line. Kind `Method` renders sensibly in
    // the outline; the detail carries the full `test "name"` form.
    for t in &program.tests {
        let (col, end_col) = name_col_on_line(tok_info, "test", t.line);
        out.push(Symbol {
            name: t.name.clone(),
            kind: SymbolKind::Method,
            line: t.line,
            col,
            end_col,
            detail: format!("test {:?}", t.name),
            doc: t.doc.clone(),
            file: None,
        });
    }

    // Benches (RFC-0055): show each `bench "name"` block in the outline, exactly
    // like tests — anchored on the `bench` keyword, kind Method, detail carrying
    // the full `bench "name"` form.
    for b in &program.benches {
        let (col, end_col) = name_col_on_line(tok_info, "bench", b.line);
        out.push(Symbol {
            name: b.name.clone(),
            kind: SymbolKind::Method,
            line: b.line,
            col,
            end_col,
            detail: format!("bench {:?}", b.name),
            doc: b.doc.clone(),
            file: None,
        });
    }

    out
}

/// Index the declarations the root document imports, out of the fully linked
/// program (RFC-0010). Only names the root's `import`s actually bring into
/// scope are indexed — plus what they imply: an imported enum's variants and an
/// imported protocol's methods (per-module visibility works the same way).
/// Columns are 0 (the foreign file's token stream isn't at hand — jump targets
/// land on the declaration line), and `file` carries the source module so the
/// LSP can build a cross-file `Location`.
fn index_imported_symbols(
    root: &ast::Program,
    linked: &ast::Program,
    origins: &OriginIndex,
) -> Vec<Symbol> {
    // Map each imported ORIGINAL decl name to the LOCAL name the root refers to
    // it by (the alias, or the original for a bare import — RFC-0022). Linked
    // decls are matched by original; the emitted symbol is keyed by the local
    // name so it lines up with the root's tokens, and an aliased binding notes
    // `— alias of <original>` in its hover detail.
    let mut local_of: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    for imp in &root.imports {
        for n in &imp.names {
            local_of.insert(n.original.as_str(), n.local());
        }
    }
    if local_of.is_empty() {
        return Vec::new();
    }
    let alias_note = |local: &str, original: &str, detail: String| -> String {
        if local == original {
            detail
        } else {
            format!("{detail}\n\n— alias of `{original}`")
        }
    };
    let mut out = Vec::new();

    for f in &linked.functions {
        if let Some(file) = &f.module {
            if let Some(&local) = local_of.get(f.name.as_str()) {
                out.push(Symbol {
                    name: local.to_string(),
                    kind: SymbolKind::Function,
                    line: f.line,
                    col: 0,
                    end_col: 0,
                    detail: alias_note(local, &f.name, function_detail(f)),
                    doc: f.doc.clone(),
                    file: Some(file.clone()),
                });
            }
        }
    }

    for p in &linked.protocols {
        if let Some(file) = &p.module {
            if let Some(&local) = local_of.get(p.name.as_str()) {
                out.push(Symbol {
                    name: local.to_string(),
                    kind: SymbolKind::Type,
                    line: p.line,
                    col: 0,
                    end_col: 0,
                    detail: alias_note(local, &p.name, protocol_detail(p)),
                    doc: p.doc.clone(),
                    file: Some(file.clone()),
                });
                for m in &p.methods {
                    out.push(Symbol {
                        name: m.name.clone(),
                        kind: SymbolKind::Method,
                        line: m.line,
                        col: 0,
                        end_col: 0,
                        detail: method_sig_detail(m),
                        doc: m.doc.clone(),
                        file: Some(file.clone()),
                    });
                }
            }
        }
    }

    for t in &linked.type_decls {
        // Same exclusions as the root indexer: parser-injected builtins
        // (line == 0) and synthetic inline-refinement types (`User.age`).
        if t.line == 0 || t.name.contains('.') {
            continue;
        }
        if let Some(file) = &t.module {
            if let Some(&local) = local_of.get(t.name.as_str()) {
                out.push(Symbol {
                    name: local.to_string(),
                    kind: SymbolKind::Type,
                    line: t.line,
                    col: 0,
                    end_col: 0,
                    detail: alias_note(local, &t.name, type_decl_detail(t, &linked.type_decls)),
                    doc: t.doc.clone(),
                    file: Some(file.clone()),
                });
                if let Type::Enum(variants) = &t.base {
                    for v in variants {
                        out.push(Symbol {
                            name: v.name.clone(),
                            kind: SymbolKind::Variant,
                            line: t.line,
                            col: 0,
                            end_col: 0,
                            detail: variant_detail(&t.name, v),
                            doc: None,
                            file: Some(file.clone()),
                        });
                    }
                }
            }
        }
    }

    // RFC-0073 M3: a name imported from a GENERATED module carries a banner as
    // its "file", which is nowhere to jump to. Where the module's map claims the
    // symbol, it stands for a real declaration instead — under the ORIGINAL
    // name, since the map is keyed by what the generator emitted and not by the
    // alias the importer chose.
    let original_of: std::collections::HashMap<&str, &str> =
        local_of.iter().map(|(orig, local)| (*local, *orig)).collect();
    for s in &mut out {
        if let Some(module) = s.file.clone() {
            let generated =
                original_of.get(s.name.as_str()).copied().unwrap_or(s.name.as_str()).to_string();
            origins.apply(&module, &generated, s);
        }
    }

    out
}

/// Index the document's `import * as ns` bindings (RFC-0027) and the exported
/// declarations each exposes. Each namespace's source module is resolved through
/// the loader (aligning the module graph's root targets with the root's imports),
/// then parsed for its exported decls — so members show ORIGINAL names (not the
/// loader's collision-rename symbols) with correct source lines for jumping in.
fn index_namespaces(
    graph: &crate::loader::ModuleGraph,
    root: &ast::Program,
    linker: Option<(
        &str,
        &crate::loader::LoadOptions,
        &dyn crate::loader::ModuleResolver,
    )>,
    origins: &OriginIndex,
) -> Vec<NamespaceInfo> {
    let Some((root_path, opts, resolver)) = linker else {
        return Vec::new();
    };
    if !root.imports.iter().any(|i| i.namespace.is_some()) {
        return Vec::new();
    }
    // The root module's resolved import targets, in `root.imports` order —
    // carrying each module's synthesized source, since a namespace may name a
    // GENERATED module (`import * as t from i18n("./strings")`) whose key is a
    // banner no resolver can read (RFC-0051 §2).
    // The graph comes from the load `analyze_inner` already performed — deriving
    // it here meant a second complete load, generators included, per keystroke.
    let _ = resolver;
    let _ = opts;
    let root_key = crate::loader::normalize(root_path);
    let Some((_, targets, _)) = graph.iter().find(|(k, _, _)| *k == root_key) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (imp, target) in root.imports.iter().zip(targets) {
        if let Some(ns) = &imp.namespace {
            let gen = graph
                .iter()
                .find(|(k, _, _)| k == target)
                .and_then(|(_, _, g)| g.as_deref());
            let mut members = namespace_members(target, resolver, gen);
            for m in &mut members {
                let generated = m.name.clone();
                origins.apply(target, &generated, m);
            }
            out.push(NamespaceInfo { name: ns.clone(), members });
        }
    }
    out
}

/// The exported declarations of `target` as hover/goto-ready [`Symbol`]s. Parses
/// the target's own source (so names/lines are the module's, and a collision
/// rename in the linked program never leaks into what the editor shows). A target
/// that can't be read and has no `gen_source` yields no members — completion
/// after that `ns.` simply offers nothing.
///
/// RFC-0051 §2: `gen_source` is the SYNTHESIZED text when `target` is a
/// generated module (its key is a banner, not a path). Its members hover but
/// carry no `file`, since there is nothing on disk to jump to — which is
/// exactly what makes `t.appTagline` (an `i18n(..)` namespace) resolve at last,
/// docs (the generated translation) included.
fn namespace_members(
    target: &str,
    resolver: &dyn crate::loader::ModuleResolver,
    gen_source: Option<&str>,
) -> Vec<Symbol> {
    let text = match gen_source {
        Some(g) => g.to_string(),
        None => match resolver.read(target) {
            Ok(t) => t,
            Err(_) => return Vec::new(),
        },
    };
    let file = |t: &str| -> Option<String> {
        if gen_source.is_some() { None } else { Some(t.to_string()) }
    };
    let Ok(tokens) = lexer::lex(&text) else {
        return Vec::new();
    };
    let (program, _errs) = parser::parse_accum(tokens);
    let mut out = Vec::new();
    for f in &program.functions {
        if f.exported {
            out.push(Symbol {
                name: f.name.clone(),
                kind: SymbolKind::Function,
                line: f.line,
                col: 0,
                end_col: 0,
                detail: function_detail(f),
                doc: f.doc.clone(),
                file: file(target),
            });
        }
    }
    for p in &program.protocols {
        if p.exported {
            out.push(Symbol {
                name: p.name.clone(),
                kind: SymbolKind::Type,
                line: p.line,
                col: 0,
                end_col: 0,
                detail: protocol_detail(p),
                doc: p.doc.clone(),
                file: file(target),
            });
        }
    }
    for t in &program.type_decls {
        if t.line == 0 || t.name.contains('.') || !t.exported {
            continue;
        }
        out.push(Symbol {
            name: t.name.clone(),
            kind: SymbolKind::Type,
            line: t.line,
            col: 0,
            end_col: 0,
            detail: type_decl_detail(t, &program.type_decls),
            doc: t.doc.clone(),
            file: file(target),
        });
        if let Type::Enum(variants) = &t.base {
            for v in variants {
                out.push(Symbol {
                    name: v.name.clone(),
                    kind: SymbolKind::Variant,
                    line: t.line,
                    col: 0,
                    end_col: 0,
                    detail: variant_detail(&t.name, v),
                    doc: None,
                    file: file(target),
                });
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// generated symbols → their declarations (RFC-0073 M3)
// ---------------------------------------------------------------------------

/// The symbol maps of every generated module the linked program reached, plus
/// the `///` on each declaration they name.
///
/// A generated stub has no source file and no doc of its own: before this, a
/// `client()` export hovered as its own signature and went nowhere on
/// go-to-definition, because the "file" a generated declaration carries is a
/// banner and not a path. The map is what supplies both halves — where the
/// declaration is, and what it says about itself.
///
/// Docs are read once per ORIGIN FILE rather than once per symbol: a client's
/// forty stubs come from a handful of api modules, and `client()` is
/// server-blind, so those modules are not in the linked program to borrow from.
#[derive(Default)]
pub(crate) struct OriginIndex {
    /// Generated module banner → its mapped symbols, by generated name.
    maps: std::collections::HashMap<String, std::collections::HashMap<String, MappedSymbol>>,
    /// `(origin file, declaration name)` → its `///`.
    docs: std::collections::HashMap<(String, String), String>,
    /// Every mapped symbol, flattened — what [`Analysis::symbol_maps`] exposes.
    all: Vec<MappedSymbol>,
}

impl OriginIndex {
    fn build(graph: &crate::loader::ModuleGraph, resolver: &dyn crate::loader::ModuleResolver) -> Self {
        let mut out = OriginIndex::default();
        for (key, _, gen) in graph {
            let Some(src) = gen else { continue };
            let syms = crate::symbolmap::read(src);
            if syms.is_empty() {
                continue;
            }
            let mut by_name = std::collections::HashMap::new();
            for s in syms {
                out.all.push(s.clone());
                by_name.insert(s.name.clone(), s);
            }
            out.maps.insert(key.clone(), by_name);
        }
        let mut files: Vec<&str> = out.all.iter().map(|s| s.file.as_str()).collect();
        files.sort_unstable();
        files.dedup();
        for file in files {
            let Ok(text) = resolver.read(file) else { continue };
            let Ok(tokens) = lexer::lex(&text) else { continue };
            let (program, _) = parser::parse_accum(tokens);
            for f in &program.functions {
                if let Some(d) = &f.doc {
                    out.docs.insert((file.to_string(), f.name.clone()), d.clone());
                }
            }
            for t in &program.type_decls {
                if let Some(d) = &t.doc {
                    out.docs.insert((file.to_string(), t.name.clone()), d.clone());
                }
            }
        }
        out
    }

    fn get(&self, module: &str, name: &str) -> Option<&MappedSymbol> {
        self.maps.get(module)?.get(name)
    }

    /// Rewrite a generated module's [`Symbol`] to stand for the declaration it
    /// was generated from: the jump target becomes the declaration, the doc is
    /// borrowed from it, and the derived wire facts join the detail.
    fn apply(&self, module: &str, generated_name: &str, sym: &mut Symbol) {
        let Some(m) = self.get(module, generated_name) else { return };
        sym.line = m.line;
        sym.col = m.col;
        sym.end_col = if m.col == 0 { 0 } else { m.col + m.decl.chars().count() };
        sym.file = Some(m.file.clone());
        // The note goes in the DOC and not the detail, so a hover reads
        // signature → what the declaration says about itself → where it is and
        // what it is mounted at. A generated stub carries no `///` of its own,
        // so the borrowed one is almost always the only one.
        let borrowed = sym
            .doc
            .clone()
            .or_else(|| self.docs.get(&(m.file.clone(), m.decl.clone())).cloned());
        sym.doc = Some(match borrowed {
            Some(d) => format!("{}\n\n{}", d.trim_end(), origin_note(m)),
            None => origin_note(m),
        });
    }
}

/// The hover lines a mapped symbol adds: its derived wire facts, and where the
/// declaration it stands for is written.
pub(crate) fn origin_note(m: &MappedSymbol) -> String {
    let mut out = String::new();
    if let Some(route) = m.route_line() {
        out.push_str(&route);
        out.push_str("\n\n");
    }
    let at = if m.line == 0 {
        short_path(&m.file)
    } else {
        format!("{}:{}", short_path(&m.file), m.line)
    };
    out.push_str(&format!("— generated from `{}` in {at}", m.decl));
    out
}

/// The tail of a module path, for a hover that must fit on a line. The LSP keys
/// modules by absolute path, and `N:/lang/examples/bin/server/api/pastes.vyrn`
/// tells a reader nothing the last three segments do not.
///
// ponytail: three segments is a guess that happens to be exactly right for an
// `<app>/server/api/x.vyrn` corpus; a project-relative path would need the
// project root threaded down here, which nothing else in this module has.
pub(crate) fn short_path(file: &str) -> String {
    let parts: Vec<&str> = file.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() <= 3 {
        return file.to_string();
    }
    format!("…/{}", parts[parts.len() - 3..].join("/"))
}

// ---------------------------------------------------------------------------
// local-binding indexing (params / lets / for-vars)
// ---------------------------------------------------------------------------

/// Index every function's local bindings: its parameters, every `let` in its
/// body (annotated or not — unannotated ones still get go-to-definition), and
/// every `for`-in loop variable. Methods (`impl` blocks) are functions too;
/// protocol methods have no body and are skipped.
fn index_locals(
    program: &ast::Program,
    tok_info: &[TokenInfo],
    let_types: &std::collections::HashMap<(usize, String), Type>,
) -> Vec<LocalBinding> {
    let mut out = Vec::new();
    for f in &program.functions {
        index_function_locals(f, tok_info, let_types, &mut out);
    }
    for imp in &program.impls {
        for m in &imp.methods {
            index_function_locals(m, tok_info, let_types, &mut out);
        }
    }
    out
}

/// One function's params + body bindings.
fn index_function_locals(
    f: &Function,
    tok_info: &[TokenInfo],
    let_types: &std::collections::HashMap<(usize, String), Type>,
    out: &mut Vec<LocalBinding>,
) {
    // Params: name on the function's line (v0 signatures are single-line; if a
    // param name isn't found there, fall back to an unknown column — the binding
    // still resolves by name).
    for p in &f.params {
        let (col, end_col) = name_col_on_line(tok_info, &p.name, f.line);
        out.push(LocalBinding {
            name: p.name.clone(),
            kind: LocalKind::Param,
            ty: Some(p.ty.clone()),
            line: f.line,
            col,
            end_col,
            fn_line: f.line,
        });
    }
    collect_lets(&f.body, f.line, tok_info, let_types, out);
}

/// Walk a block recursively, collecting `let` and `for`-in bindings. `if` and
/// `while` bodies (and `else` blocks) are recursed so bindings inside nested
/// blocks are indexed at their own line. The checker's `let_types` table fills
/// in the inferred type for unannotated `let`s (and the element type for
/// `for`-vars); an annotated `let` keeps its AST annotation (the table holds
/// the same value). A binding after a same-function error isn't in the table, so
/// it falls back to the AST annotation (None for unannotated → no type shown).
fn collect_lets(
    block: &Block,
    fn_line: usize,
    tok_info: &[TokenInfo],
    let_types: &std::collections::HashMap<(usize, String), Type>,
    out: &mut Vec<LocalBinding>,
) {
    for stmt in &block.stmts {
        match stmt {
            Stmt::Let {
                name,
                mutable,
                ty,
                line,
                ..
            } => {
                // Synthetic desugar temporaries (e.g. `ps[]`, from `a[i].f = v`)
                // are unspellable — they contain characters no real identifier
                // can — and have no source token; never surface them as
                // hover/outline/completion locals.
                if name.starts_with('@') || name.contains('[') {
                    continue;
                }
                let (col, end_col) = name_col_on_line(tok_info, name, *line);
                // Prefer the checker's retained type (covers unannotated lets);
                // fall back to the AST annotation.
                let inferred = let_types
                    .get(&(*line, name.clone()))
                    .cloned()
                    .or_else(|| ty.clone());
                out.push(LocalBinding {
                    name: name.clone(),
                    kind: LocalKind::Let { mutable: *mutable },
                    ty: inferred,
                    line: *line,
                    col,
                    end_col,
                    fn_line,
                });
            }
            Stmt::ForIn {
                var, body, line, ..
            } => {
                let (col, end_col) = name_col_on_line(tok_info, var, *line);
                // The element type is inferred by the checker and retained in
                // `let_types`; fall back to None if it isn't there.
                let elem_ty = let_types.get(&(*line, var.clone())).cloned();
                out.push(LocalBinding {
                    name: var.clone(),
                    kind: LocalKind::ForVar,
                    ty: elem_ty,
                    line: *line,
                    col,
                    end_col,
                    fn_line,
                });
                collect_lets(body, fn_line, tok_info, let_types, out);
            }
            Stmt::If {
                then_block,
                else_block,
                ..
            } => {
                collect_lets(then_block, fn_line, tok_info, let_types, out);
                if let Some(eb) = else_block {
                    collect_lets(eb, fn_line, tok_info, let_types, out);
                }
            }
            Stmt::IfLet {
                pattern,
                then_block,
                else_block,
                line,
                ..
            } => {
                // `if let` binders are real locals scoped to the then-block
                // (RFC-0060): surface each for hover / go-to-def / completion /
                // highlight, typed from the checker's retained payload types.
                for b in crate::movecheck::pattern_bindings(pattern) {
                    let (col, end_col) = name_col_on_line(tok_info, b, *line);
                    let ty = let_types.get(&(*line, b.to_string())).cloned();
                    out.push(LocalBinding {
                        name: b.to_string(),
                        kind: LocalKind::Let { mutable: false },
                        ty,
                        line: *line,
                        col,
                        end_col,
                        fn_line,
                    });
                }
                collect_lets(then_block, fn_line, tok_info, let_types, out);
                if let Some(eb) = else_block {
                    collect_lets(eb, fn_line, tok_info, let_types, out);
                }
            }
            Stmt::While { body, .. } => collect_lets(body, fn_line, tok_info, let_types, out),
            // Assign/SetField/Return/Drop/Expr reference existing bindings; no new ones.
            _ => {}
        }
    }
}

/// Build a [`Resolution`] for a local binding.
/// RFC-0051 §1: hover text = the signature, then the declaration's `///` doc
/// (already markdown, passed through verbatim) when it has one.
fn with_doc(detail: &str, doc: &Option<String>) -> String {
    match doc {
        Some(d) if !d.trim().is_empty() => format!("{detail}\n\n{}", d.trim_end()),
        _ => detail.to_string(),
    }
}

/// RFC-0051 §3: the declaration of the user record/enum named `name`, rendered
/// beneath a value's hover so `error: PageError` also shows the shape. Bounded
/// on purpose: the type's own one-line declaration (+ its doc), never a
/// recursive expansion of the field types. `None` when the name isn't an
/// indexed user type (a builtin like `Int64`, or a type from a module the root
/// doesn't import).
fn type_structure(analysis: &Analysis, name: &str) -> Option<String> {
    let sym = analysis
        .symbols
        .iter()
        .filter(|s| s.kind == SymbolKind::Type && s.name == name)
        .max_by_key(|s| (s.file.is_none(), s.line))?;
    // Only structural declarations (`type T = { .. }` / `A | B`) add information
    // a value's hover doesn't already carry; a protocol is not a value's type.
    if !sym.detail.starts_with("type ") {
        return None;
    }
    Some(with_doc(&format!("```vyrn\n{}\n```", sym.detail), &sym.doc))
}

/// The user type a binding's hover should expand (RFC-0051 §3): the binding's
/// own named type, or the element type of an array/map of a named type.
fn structural_name(ty: &Type) -> Option<&str> {
    match ty {
        Type::Named(n) => Some(n.as_str()),
        Type::Array(inner) => structural_name(inner),
        Type::ArrayN(inner, _) => structural_name(inner),
        Type::SmallArray(inner, _) => structural_name(inner),
        Type::Map(_, v) => structural_name(v),
        _ => None,
    }
}

fn local_resolution(analysis: &Analysis, b: &LocalBinding) -> Resolution {
    let hover = match b.ty.as_ref().and_then(structural_name) {
        Some(n) => match type_structure(analysis, n) {
            Some(s) => format!("{}\n\n{}", local_detail(b), s),
            None => local_detail(b),
        },
        None => local_detail(b),
    };
    // RFC-0087 U1: three bindings of one shape can have opposite outcomes, and
    // nothing in the source says which. Matched on the DECLARATION line, so it is
    // this binding's answer and not a same-named one from another scope.
    let hover = match analysis.memory.iter().find(|m| m.name == b.name && m.line == b.line) {
        Some(m) => format!("{hover}\n\nmemory: {}", m.text),
        None => hover,
    };
    Resolution {
        name: b.name.clone(),
        kind: match b.kind {
            LocalKind::Param => SymbolKind::Param,
            LocalKind::Let { .. } | LocalKind::ForVar => SymbolKind::Local,
        },
        target_line: b.line,
        target_col: b.col,
        target_end_col: b.end_col,
        target_file: None,
        hover,
        definition: true,
    }
}

/// Hover text for a local binding. Params show `name: Type`; annotated lets
/// show `let [mut] name: Type`; unannotated lets / for-vars show the binding
/// without a type (the type is inferred and not retained here).
fn local_detail(b: &LocalBinding) -> String {
    match b.kind {
        LocalKind::Param => format!("{}: {}", b.name, type_to_string(b.ty.as_ref().unwrap())),
        LocalKind::Let { mutable } => match (&b.ty, mutable) {
            (Some(ty), true) => format!("let mut {}: {}", b.name, type_to_string(ty)),
            (Some(ty), false) => format!("let {}: {}", b.name, type_to_string(ty)),
            (None, true) => format!("let mut {}", b.name),
            (None, false) => format!("let {}", b.name),
        },
        LocalKind::ForVar => match &b.ty {
            Some(ty) => format!("for {}: {}", b.name, type_to_string(ty)),
            None => format!("for {}", b.name),
        },
    }
}

// ---------------------------------------------------------------------------
// detail (hover) renderers
// ---------------------------------------------------------------------------

fn function_detail(f: &Function) -> String {
    let params = f
        .params
        .iter()
        .map(|p| format!("{}: {}", p.name, type_to_string(&p.ty)))
        .collect::<Vec<_>>()
        .join(", ");
    let tp = if f.type_params.is_empty() {
        String::new()
    } else {
        format!("<{}>", f.type_params.join(", "))
    };
    // An `extern` import (RFC-0012 M1) shows the `extern fn` prefix, and an
    // `export extern` (M2) the `export extern fn` prefix, so hover makes the
    // JS-boundary crossing (and its direction) obvious.
    // A `mut fn` (RFC-0074 M4a) shows its marker for the same reason: it is what
    // a projection reads to call the procedure a Mutation, and a reader who
    // cannot see it on hover is back to guessing from the name.
    let kw = if f.is_export_extern {
        "export extern fn"
    } else if f.is_extern {
        "extern fn"
    } else if f.is_mut {
        "mut fn"
    } else {
        "fn"
    };
    format!(
        "{} {}{}({}) -> {}",
        kw,
        f.name,
        tp,
        params,
        type_to_string(&f.ret)
    )
}

/// Hover text for a module-state binding (RFC-0013), e.g. `let mut hits: Int64`.
/// The type is the annotation when present, else a best-effort inference from a
/// literal initializer (the common `let mut hits = 0` case), else omitted.
fn global_detail(g: &GlobalDecl) -> String {
    let kw = if g.mutable { "let mut" } else { "let" };
    let ty = g.ty.clone().or_else(|| infer_literal_type(&g.init));
    match ty {
        Some(t) => format!("{} {}: {}", kw, g.name, type_to_string(&t)),
        None => format!("{} {}", kw, g.name),
    }
}

/// A best-effort type for a literal initializer, for hover only (the checker is
/// authoritative). Covers scalars and homogeneous array literals of scalars.
fn infer_literal_type(e: &Expr) -> Option<Type> {
    match e {
        Expr::Int(_) => Some(Type::Int),
        // A byte literal (RFC-0057) defaults to `UInt8` — hover shows that.
        Expr::Byte(_) => Some(Type::IntN {
            bits: 8,
            signed: false,
        }),
        Expr::Float(_) => Some(Type::Float),
        Expr::Bool(_) => Some(Type::Bool),
        Expr::Str(_) => Some(Type::Str),
        Expr::Unary { expr, .. } => infer_literal_type(expr),
        Expr::ArrayLit { elems, .. } => elems
            .first()
            .and_then(infer_literal_type)
            .map(|t| Type::Array(Box::new(t))),
        _ => None,
    }
}

/// The `///` on the protocol signature that `method` implements, if any.
///
/// A `///` on the signature documents EVERY implementation of that method, so an
/// impl body repeating it would be a second copy to keep in step — and a bare
/// `cacheFor` hovers on the LAST declaration of the name, which is the impl.
/// Both hover and `.`-completion borrow the signature's doc here rather than
/// leaving the implementation undocumented.
fn signature_doc(protocols: &[ProtocolDecl], protocol: &str, method: &str) -> Option<String> {
    protocols
        .iter()
        .find(|p| p.name == protocol)
        .and_then(|p| p.methods.iter().find(|m| m.name == method))
        .and_then(|m| m.doc.clone())
}

fn method_sig_detail(m: &MethodSig) -> String {
    // MethodSig.params are types only (names are dropped by the parser); the
    // receiver `self` is implied and prepended. Capabilities ARE shown — a
    // reader hovering `insert` wants to know it takes the container by `modify`
    // and the element by `consume`, which is the whole discipline of the call.
    let cap = |c: Capability, t: String| match c {
        Capability::Read => t,
        Capability::Modify => format!("modify {t}"),
        Capability::Consume => format!("consume {t}"),
        Capability::Share => format!("share {t}"),
    };
    let mut ps = vec![cap(m.recv, "self".to_string())];
    ps.extend(m.params.iter().enumerate().map(|(i, t)| {
        cap(
            m.param_caps.get(i).copied().unwrap_or(Capability::Read),
            type_to_string(t),
        )
    }));
    format!("fn {}({}) -> {}", m.name, ps.join(", "), type_to_string(&m.ret))
}

fn protocol_detail(p: &ProtocolDecl) -> String {
    let ms = p
        .methods
        .iter()
        .map(method_sig_detail)
        .collect::<Vec<_>>()
        .join("; ");
    if ms.is_empty() {
        format!("protocol {}", p.name)
    } else {
        format!("protocol {} {{ {} }}", p.name, ms)
    }
}

/// Render one record field as the user wrote it — a synthetic
/// inline-refinement field type (`User.age`) is expanded back to
/// `age: Int64 where value >= 18`.
fn field_detail(f: &ast::Field, all: &[TypeDecl]) -> String {
    if let Type::Named(n) = &f.ty {
        if n.contains('.') {
            if let Some(d) = all.iter().find(|d| d.name == *n) {
                if let Some(pred) = &d.predicate {
                    return format!(
                        "{}: {} where {}",
                        f.name,
                        type_to_string(&d.base),
                        crate::checker::pred_summary(pred)
                    );
                }
            }
        }
    }
    format!("{}: {}", f.name, type_to_string(&f.ty))
}

fn type_decl_detail(t: &TypeDecl, all: &[TypeDecl]) -> String {
    match &t.base {
        Type::Enum(vs) => {
            let arms = vs.iter().map(variant_arm).collect::<Vec<_>>().join(" | ");
            format!("type {} = {}", t.name, arms)
        }
        Type::Record(fields) => {
            let fs = fields
                .iter()
                .map(|f| field_detail(f, all))
                .collect::<Vec<_>>()
                .join(", ");
            format!("type {} = {{ {} }}", t.name, fs)
        }
        _ => {
            let s = type_to_string(&t.base);
            if t.predicate.is_some() {
                format!("type {} = {} (validated)", t.name, s)
            } else {
                format!("type {} = {}", t.name, s)
            }
        }
    }
}

fn variant_arm(v: &EnumVariant) -> String {
    if v.payload.is_empty() {
        v.name.clone()
    } else {
        format!(
            "{}({})",
            v.name,
            v.payload
                .iter()
                .map(type_to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

fn variant_detail(enum_name: &str, v: &EnumVariant) -> String {
    if v.payload.is_empty() {
        format!("variant of {}: {}", enum_name, v.name)
    } else {
        format!(
            "variant of {}: {}({})",
            enum_name,
            v.name,
            v.payload
                .iter()
                .map(type_to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

fn type_to_string(ty: &Type) -> String {
    // One source of truth: the AST's `Display` impl (the user-facing type
    // spelling). Enums keep the richer per-variant arm rendering for hovers.
    match ty {
        Type::Enum(vs) => {
            let arms = vs.iter().map(variant_arm).collect::<Vec<_>>().join(" | ");
            format!("{{ {} }}", arms)
        }
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// doc model (RFC-0065: `vyrn doc`)
// ---------------------------------------------------------------------------

/// One exported declaration to document (RFC-0065): its rendered signature line
/// (the same typed view hover shows) and its `///` block verbatim.
#[derive(Debug, Clone)]
pub struct DocExport {
    pub name: String,
    pub kind: SymbolKind,
    /// 1-based declaration line — the sort key for declaration order.
    pub line: usize,
    /// The rendered signature: `fn route(req: Request) -> Response`,
    /// `type Paste = { .. }`, `protocol Show { fn show(self) -> String }`.
    pub signature: String,
    /// The declaration's `///` documentation (markdown), verbatim; `None` when it
    /// carries no doc comment.
    pub doc: Option<String>,
    /// The DOCUMENTED members of this export — `(signature, doc)` per protocol
    /// method that carries a `///` block. Empty for everything else, and for a
    /// protocol whose methods are undocumented: the entry exists to carry the
    /// prose, and a member with none has nothing the one-line protocol signature
    /// above it does not already say.
    pub members: Vec<(String, String)>,
}

/// A module's documentation model for `vyrn doc` (RFC-0065): the detached
/// file-header `///` block, then every EXPORT in declaration order. Private
/// declarations, tests, and benches are omitted. Built from the parse alone (no
/// checker/linker), so a module documents even with unresolved imports — the
/// signatures are exactly the declared types the parser recorded.
#[derive(Debug, Clone)]
pub struct ModuleDoc {
    /// The leading, detached `///` block that belongs to the file (not to the
    /// first declaration), per the established detached-block rule.
    pub header_doc: Option<String>,
    pub exports: Vec<DocExport>,
}

/// The module-header doc: the first maximal run of `///` lines at the top of the
/// file, but ONLY when it is DETACHED from what follows (a blank-line gap, or the
/// file has no declarations). A `///` block sitting directly above a declaration
/// belongs to that declaration — never to the module — so it is not a header.
/// Mirrors the parser's `take_docs` detachment rule so the two never disagree.
fn module_header_doc(tokens: &[lexer::Token]) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut last_line = 0usize;
    for t in tokens {
        match &t.tok {
            lexer::Tok::Doc(s) => {
                // A gap inside the leading run closes the first block: it is
                // detached from everything after it, so it is the header.
                if !lines.is_empty() && t.line > last_line + 1 {
                    return Some(lines.join("\n"));
                }
                lines.push(s.clone());
                last_line = t.line;
            }
            lexer::Tok::Eof => break,
            _ => {
                if lines.is_empty() {
                    return None; // code (or an import) leads the file — no header
                }
                // The block is the header only if a blank line detaches it from
                // this first real token; otherwise it attaches to the declaration.
                return if t.line > last_line + 1 {
                    Some(lines.join("\n"))
                } else {
                    None
                };
            }
        }
    }
    // The file is nothing but a leading `///` block (and EOF): it is the header.
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

/// Build the documentation model for one module's source (RFC-0065). Parses the
/// text (recovery-tolerant, like the LSP) and collects every exported function,
/// protocol, and type — each with the signature hover renders and its `///` block
/// verbatim — sorted by declaration line so output is stable and reads top-down.
pub fn module_doc(source: &str) -> ModuleDoc {
    let Ok(tokens) = lexer::lex(source) else {
        return ModuleDoc { header_doc: None, exports: Vec::new() };
    };
    let header_doc = module_header_doc(&tokens);
    let (program, _errs) = parser::parse_accum(tokens);
    let mut exports: Vec<DocExport> = Vec::new();
    for f in &program.functions {
        if f.exported {
            exports.push(DocExport {
                name: f.name.clone(),
                kind: SymbolKind::Function,
                line: f.line,
                signature: function_detail(f),
                doc: f.doc.clone(),
                members: Vec::new(),
            });
        }
    }
    for p in &program.protocols {
        if p.exported {
            exports.push(DocExport {
                name: p.name.clone(),
                kind: SymbolKind::Type,
                line: p.line,
                signature: protocol_detail(p),
                doc: p.doc.clone(),
                members: p
                    .methods
                    .iter()
                    .filter_map(|m| {
                        m.doc.clone().map(|d| (method_sig_detail(m), d))
                    })
                    .collect(),
            });
        }
    }
    for t in &program.type_decls {
        // Skip synthetic decls: `schemaOf`-produced records (line 0) and the
        // dotted inline-refinement field types — neither is a real export.
        if t.line == 0 || t.name.contains('.') || !t.exported {
            continue;
        }
        exports.push(DocExport {
            name: t.name.clone(),
            kind: SymbolKind::Type,
            line: t.line,
            signature: type_decl_detail(t, &program.type_decls),
            doc: t.doc.clone(),
            members: Vec::new(),
        });
    }
    // Declaration order: sort by source line (stable across runs). Ties (two
    // decls on one line — synthetic only) fall back to name for determinism.
    exports.sort_by(|a, b| a.line.cmp(&b.line).then_with(|| a.name.cmp(&b.name)));
    ModuleDoc { header_doc, exports }
}

// ---------------------------------------------------------------------------
// semantic tokens (RFC-0047 §1)
// ---------------------------------------------------------------------------

/// The kind an identifier resolves to for semantic highlighting. A superset of
/// the categories the checker already tracks, mapped 1:1 onto the LSP standard
/// token-type set by the server (`namespace`/`type`/`enumMember`/`parameter`/
/// `variable`/`property`/`function`/`method`/`macro`). This is what lets the
/// editor colour a function call differently from a variable — the distinction
/// TextMate cannot make without name resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemKind {
    /// An `import * as ns` binding, or a `ns.` qualifier.
    Namespace,
    /// A type or enum name (incl. imported types).
    Type,
    /// An enum variant / option-result constructor (`Circle`, `Some`, `Ok`).
    EnumMember,
    /// A function parameter.
    Parameter,
    /// A `let` / `for`-var local, or module state.
    Variable,
    /// A record field accessed as a member.
    Property,
    /// A function (definition, call, or imported fn).
    Function,
    /// A protocol/builtin method.
    Method,
    /// A compiler builtin free function (`toJson`, `slice`, `bytes`, `print`, …)
    /// — coloured distinctly from user calls.
    Macro,
}

/// Semantic-token modifiers, packed into a small struct (the server encodes them
/// as an LSP bitset). `declaration` marks the defining occurrence; `readonly`
/// marks a non-`mut` binding; `default_library` marks std / builtins.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SemMods {
    pub declaration: bool,
    pub readonly: bool,
    pub default_library: bool,
    /// RFC-0087 U1: this occurrence is where the binding's value stops being
    /// live — the move or the `drop` that takes it. Set for locals only.
    pub last_use: bool,
}

/// One classified identifier occurrence (RFC-0047 §1): its 1-based position, its
/// character length, its [`SemKind`], and its [`SemMods`].
#[derive(Debug, Clone)]
pub struct SemToken {
    pub line: usize,
    pub col: usize,
    pub len: usize,
    pub kind: SemKind,
    pub mods: SemMods,
}

/// Compiler builtin *free functions* (not the `x.method()` builtins, which are
/// handled by [`builtin_method`]). These are reserved call names the checker
/// lowers inline; colouring them `macro` reads them apart from user functions.
/// Contextual reserved words (`value`/`list` in a `where` clause) are excluded so
/// they are never mis-coloured. Kept in sync with the checker's `RESERVED` list.
static MACRO_BUILTINS: &[&str] = &[
    "print", "len", "concat", "slice", "bytes", "chars", "floatBits", "floatFromBits", "hexEncode",
    "hexDecode",
    "base64Encode", "base64Decode", "urlEncode", "urlDecode", "args", "readLine",
    "readFile", "writeFile", "renameFile", "fsyncFile", "readFileBytes",
    "stringFromBytes", "listDir", "moduleInterface", "schemaOf", "contractOf", "jsonSchema",
    "toJson", "fromJson", "assert", "assertEq", "array", "parse", "str", "panic",
];

/// Option / result constructors — builtin enum-like variants, coloured as
/// `enumMember` (same as user variants).
static CONSTRUCTOR_BUILTINS: &[&str] = &["Some", "None", "Ok", "Err"];

/// Map a [`SymbolKind`] to the semantic-token [`SemKind`].
fn sem_of_symbol_kind(k: SymbolKind) -> SemKind {
    match k {
        SymbolKind::Function => SemKind::Function,
        SymbolKind::Type => SemKind::Type,
        SymbolKind::Variant => SemKind::EnumMember,
        SymbolKind::Method => SemKind::Method,
        SymbolKind::Field => SemKind::Property,
        SymbolKind::Param => SemKind::Parameter,
        SymbolKind::Local => SemKind::Variable,
        SymbolKind::Global => SemKind::Variable,
    }
}

/// Whether an imported symbol's source file is a std-library module (so it earns
/// the `defaultLibrary` modifier).
fn is_std_file(file: &Option<String>) -> bool {
    file.as_deref().is_some_and(|f| f.contains("/std/") || f.starts_with("std/"))
}

/// Classify every identifier in the document for semantic highlighting
/// (RFC-0047 §1). Computed from the already-built [`Analysis`] — no reparse.
/// Import specifiers resolve to their imported declaration's real kind (so
/// `import { format, Locale }` yields `format`→function, `Locale`→type), exactly
/// as go-to-definition does. Tokens that resolve to nothing (keywords, unknown
/// names) are omitted, leaving them to the TextMate grammar.
pub fn semantic_tokens(analysis: &Analysis) -> Vec<SemToken> {
    let mut out = Vec::new();
    for t in &analysis.tokens {
        if t.text == "." {
            continue;
        }
        if let Some((kind, mods)) = classify_token(analysis, t) {
            out.push(SemToken {
                line: t.line,
                col: t.col,
                len: t.end_col.saturating_sub(t.col),
                kind,
                mods,
            });
        }
    }
    out
}

/// One inlay hint: a label the editor draws inside the line, without editing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlayHint {
    /// 1-based position the label is drawn at.
    pub line: usize,
    pub col: usize,
    pub label: String,
}

/// Move hints for a document (RFC-0087 U1).
///
/// A move is the one memory event with a source position of its own — block-exit
/// reclamation happens at a brace, and a leak happens nowhere. So this marks the
/// moves: at the occurrence that takes the value, `→ f(..)` says where it went.
///
/// Pure over the cached [`Analysis`], like [`semantic_tokens`]. A keystroke pays
/// nothing for it.
pub fn inlay_hints(analysis: &Analysis) -> Vec<InlayHint> {
    let mut out = Vec::new();
    for m in &analysis.memory {
        let (Some(at), Some(into)) = (m.last_use, m.moved_into.as_ref()) else { continue };
        // The occurrence of the name on the move line. A move is a use, so the
        // token is there; if the source moved on since the analysis, no hint.
        let Some(tok) = analysis
            .tokens
            .iter()
            .find(|t| t.line == at && t.text == m.name)
        else {
            continue;
        };
        // `into` is written for a diagnostic, where a name wears backticks. An
        // inlay hint is drawn in the code, where it must not.
        let into = into.trim_matches('`');
        out.push(InlayHint { line: at, col: tok.end_col, label: format!("→ {into}") });
    }
    out.sort_by_key(|h| (h.line, h.col));
    out.dedup();
    out
}

/// Classify the identifier token covering 1-based `(line, col)`, if any — the
/// entry point the `.vyx` forward-mapper uses to classify a template token
/// against a synthesized module's analysis (RFC-0047 §1). Mirrors [`resolve`]'s
/// precedence exactly, so a semantic colour always agrees with hover.
pub fn classify_at(analysis: &Analysis, line: usize, col: usize) -> Option<(SemKind, SemMods)> {
    let tok = analysis
        .tokens
        .iter()
        .find(|t| t.text != "." && t.line == line && col >= t.col && col < t.end_col)?;
    classify_token(analysis, tok)
}

// ---------------------------------------------------------------------------
// RFC-0050 §1 — scope-aware references (documentHighlight)
// ---------------------------------------------------------------------------

/// One reference to a binding, for `textDocument/documentHighlight`. `write`
/// marks the defining occurrence (the declaration); every use is a read.
#[derive(Debug, Clone)]
pub struct RefRange {
    pub line: usize,
    pub col: usize,
    pub end_col: usize,
    pub write: bool,
}

/// Whether `tok` sits in member position — immediately after a `.` on the same
/// line (`recv.tok`). Such a token references a member/field, NOT a binding of
/// the same bare name, so binding highlights must skip it.
fn is_member_position(analysis: &Analysis, tok: &TokenInfo) -> bool {
    analysis
        .tokens
        .iter()
        .any(|d| d.text == "." && d.line == tok.line && d.end_col == tok.col)
}

/// The local binding [`resolve`] would pick for a use of `name` on `line` inside
/// the function at `fn_line`: the latest same-named binding defined at or before
/// the line (line-based scope, mirroring [`resolve`]).
fn binding_for<'a>(
    analysis: &'a Analysis,
    fn_line: usize,
    name: &str,
    line: usize,
) -> Option<&'a LocalBinding> {
    analysis
        .locals
        .iter()
        .filter(|b| b.fn_line == fn_line && b.name == name && b.line <= line)
        .max_by_key(|b| b.line)
}

/// Scope-aware references to the binding under the 1-based `(line, col)` cursor
/// (RFC-0050 §1). Returns the ranges of that binding's ACTUAL occurrences — the
/// same resolution hover/definition use — never a textual word-match: a `let`/
/// param highlights only its in-scope uses (an out-of-scope same-named binding
/// in another function is excluded), a top-level symbol its real references
/// (excluded where a local shadows the name), a namespace binding its qualifier
/// occurrences. Comments are never lexed to tokens, so they never appear.
///
/// An empty result means "nothing resolves here" — the server returns it as an
/// empty highlight list so the editor does not fall back to word-matching.
pub fn references(analysis: &Analysis, line: usize, col: usize) -> Vec<RefRange> {
    let Some(tok) = analysis
        .tokens
        .iter()
        .find(|t| t.text != "." && t.line == line && col >= t.col && col < t.end_col)
    else {
        return Vec::new();
    };
    let name = tok.text.clone();
    let cursor_member = is_member_position(analysis, tok);

    // A member occurrence (`recv.member`): highlight the same member accessed
    // through the same-named receiver (namespace exports, record fields, builtin
    // methods). Kept receiver-scoped so unrelated same-named members elsewhere
    // are not swept in.
    if cursor_member {
        let recv = receiver_before_dot(analysis, tok.line, tok.col);
        let mut out = Vec::new();
        for t in &analysis.tokens {
            if t.text != name || !is_member_position(analysis, t) {
                continue;
            }
            if receiver_before_dot(analysis, t.line, t.col).as_deref() == recv.as_deref() {
                out.push(RefRange { line: t.line, col: t.col, end_col: t.end_col, write: false });
            }
        }
        return dedup_refs(out);
    }

    // A local binding (param / let / for-var) shadows everything: highlight only
    // the uses that resolve to THIS binding, and only within the same function.
    if let Some(fn_line) = enclosing_fn_line(analysis, line) {
        if let Some(target) = binding_for(analysis, fn_line, &name, line) {
            let (t_line, t_col) = (target.line, target.col);
            let mut out = Vec::new();
            for t in &analysis.tokens {
                if t.text != name || is_member_position(analysis, t) {
                    continue;
                }
                if enclosing_fn_line(analysis, t.line) != Some(fn_line) {
                    continue;
                }
                match binding_for(analysis, fn_line, &name, t.line) {
                    Some(b) if b.line == t_line && b.col == t_col => {
                        let write = t.line == t_line && t.col == t_col;
                        out.push(RefRange {
                            line: t.line,
                            col: t.col,
                            end_col: t.end_col,
                            write,
                        });
                    }
                    _ => {}
                }
            }
            return dedup_refs(out);
        }
    }

    // A namespace binding (`import * as ns` and every `ns.` qualifier): its
    // occurrences are the bare-`ns` tokens not shadowed by a local at that point.
    if analysis.namespaces.iter().any(|n| n.name == name) {
        let mut out = Vec::new();
        for t in &analysis.tokens {
            if t.text != name || is_member_position(analysis, t) {
                continue;
            }
            let shadowed = enclosing_fn_line(analysis, t.line).is_some_and(|fl| {
                analysis
                    .locals
                    .iter()
                    .any(|b| b.fn_line == fl && b.name == name && b.line <= t.line)
            });
            if shadowed {
                continue;
            }
            out.push(RefRange { line: t.line, col: t.col, end_col: t.end_col, write: false });
        }
        return dedup_refs(out);
    }

    // A top-level symbol: its real references, excluding positions where an
    // in-scope local of the same name shadows it (there the token is the local).
    if let Some(sym) = analysis
        .symbols
        .iter()
        .filter(|s| s.name == name)
        .max_by_key(|s| (s.file.is_none(), s.line))
    {
        let (s_line, s_col, s_local) = (sym.line, sym.col, sym.file.is_none());
        let mut out = references_to(analysis, &name, &[]);
        if s_local {
            for r in &mut out {
                r.write = r.line == s_line && r.col == s_col;
            }
        }
        return out;
    }

    // Unresolved (a keyword, an unknown name, a builtin): no binding to
    // highlight — an empty list, which suppresses the editor's word-match.
    Vec::new()
}

/// Every occurrence of the top-level `name` in this document, for a caller that
/// knows the name but has no cursor on it — the cross-file half of the RFC-0073
/// M4 rename, where the name is carried in from another file (or derived from a
/// symbol map) rather than read off a token.
///
/// `qualifiers` are the namespace bindings the name is ALSO reached through, so
/// `store.listPastes` counts when `store` names the declaring module and an
/// unrelated `other.listPastes` does not. The shadow rule is [`references`]'s
/// own: a bare occurrence inside a function that binds the same name locally is
/// the LOCAL, and never a reference to the declaration.
///
/// Nothing here is a textual search. The occurrences are lexer TOKENS, so a
/// mention in a comment, a substring of a longer identifier (`recentRows`), and
/// the same letters inside a string literal are all excluded by construction —
/// which is what makes it safe to run over a file this server has not linked.
pub fn references_to(analysis: &Analysis, name: &str, qualifiers: &[String]) -> Vec<RefRange> {
    let mut out = Vec::new();
    for t in &analysis.tokens {
        if t.text != name {
            continue;
        }
        if is_member_position(analysis, t) {
            let recv = receiver_before_dot(analysis, t.line, t.col);
            if !recv.is_some_and(|r| qualifiers.iter().any(|q| *q == r)) {
                continue;
            }
        } else {
            let shadowed = enclosing_fn_line(analysis, t.line).is_some_and(|fl| {
                analysis
                    .locals
                    .iter()
                    .any(|b| b.fn_line == fl && b.name == name && b.line <= t.line)
            });
            if shadowed {
                continue;
            }
        }
        out.push(RefRange { line: t.line, col: t.col, end_col: t.end_col, write: false });
    }
    dedup_refs(out)
}

/// Drop duplicate ranges (overlapping scopes could otherwise double-list a
/// position), keeping the first — which retains a `write` flag over a later
/// `read` at the same spot.
fn dedup_refs(mut refs: Vec<RefRange>) -> Vec<RefRange> {
    refs.sort_by_key(|r| (r.line, r.col, !r.write));
    refs.dedup_by_key(|r| (r.line, r.col));
    refs
}

// ---------------------------------------------------------------------------
// RFC-0050 §2 — import-path string under the cursor
// ---------------------------------------------------------------------------

/// If the 1-based `(line, col)` cursor is inside an import's SOURCE STRING — a
/// plain specifier (`"./store"`, `"std/time"`) or a string argument of a
/// generator-call import (`i18n("../strings")`, `componentsThemed("./a","./b")`)
/// — return the specifier text (RFC-0050 §2). The server then resolves it to a
/// file through the loader. `None` when the cursor is not on an import string.
///
/// Import statements may span several lines (`import {\n ..\n} from "path"`), so
/// membership is decided by statement span, not the physical line: the greatest
/// import-or-declaration line at/above the string must be an import line.
pub fn import_spec_at(source: &str, line: usize, col: usize) -> Option<String> {
    let toks = lexer::lex(source).ok()?;
    // The string-literal token whose span contains the cursor.
    let (str_line, spec) = toks.iter().find_map(|t| {
        if t.line != line {
            return None;
        }
        if let Tok::Str(s) = &t.tok {
            let start = t.col;
            let end = t.col + s.chars().count() + 2; // + the two quotes
            if col >= start && col <= end {
                return Some((t.line, s.clone()));
            }
        }
        None
    })?;

    let (program, _errs) = parser::parse_accum(toks);
    // Statement-start lines: every top-level declaration AND every import. The
    // greatest one at/above the string's line is the statement it belongs to.
    let mut stmt_lines: Vec<(usize, bool)> = decl_lines(&program)
        .into_iter()
        .map(|l| (l, false))
        .collect();
    for imp in &program.imports {
        stmt_lines.push((imp.line, true));
    }
    stmt_lines.sort_by_key(|(l, _)| *l);
    let is_import = stmt_lines
        .iter()
        .rev()
        .find(|(l, _)| *l <= str_line)
        .map(|(_, is_imp)| *is_imp)
        .unwrap_or(false);
    is_import.then_some(spec)
}

/// The classification core: given an identifier [`TokenInfo`], resolve it to a
/// [`SemKind`] + [`SemMods`] using the same local → namespace → symbol → builtin
/// precedence as [`resolve`].
fn classify_token(analysis: &Analysis, tok: &TokenInfo) -> Option<(SemKind, SemMods)> {
    let line = tok.line;

    // 1. Local bindings (params / lets / for-vars) shadow everything else.
    if let Some(fn_line) = enclosing_fn_line(analysis, line) {
        if let Some(b) = analysis
            .locals
            .iter()
            .filter(|b| b.fn_line == fn_line && b.name == tok.text && b.line <= line)
            .max_by_key(|b| b.line)
        {
            let kind = match b.kind {
                LocalKind::Param => SemKind::Parameter,
                LocalKind::Let { .. } | LocalKind::ForVar => SemKind::Variable,
            };
            let readonly = matches!(b.kind, LocalKind::Let { mutable: false } | LocalKind::ForVar);
            let declaration = b.line == tok.line && b.col == tok.col;
            // RFC-0087 U1: the line where an owning value stops being live — a
            // move or a `drop`. Marked so the point is visible rather than
            // inferred by reading the rest of the body.
            let last_use = !declaration
                && analysis
                    .memory
                    .iter()
                    .any(|m| m.name == b.name && m.line == b.line && m.last_use == Some(tok.line));
            return Some((
                kind,
                SemMods { declaration, readonly, default_library: false, last_use },
            ));
        }
    }

    // 2. A member access `recv.tok`. First `ns.member` (an in-scope namespace
    //    qualifier); then a record field on a typed receiver → `property`.
    if let Some(recv) = receiver_before_dot(analysis, tok.line, tok.col) {
        let shadowed = enclosing_fn_line(analysis, line).is_some_and(|fl| {
            analysis
                .locals
                .iter()
                .any(|b| b.fn_line == fl && b.name == recv && b.line <= line)
        });
        if !shadowed {
            if let Some(nsi) = analysis.namespaces.iter().find(|n| n.name == recv) {
                if let Some(m) = nsi.members.iter().find(|m| m.name == tok.text) {
                    return Some((
                        sem_of_symbol_kind(m.kind),
                        SemMods {
                            declaration: false,
                            readonly: false,
                            default_library: is_std_file(&m.file),
                            last_use: false,
                        },
                    ));
                }
            }
        }
        // A record field on a receiver whose type we can resolve (a typed local /
        // param). `u.age` → `property`, mirroring member completion.
        if let Some(ty) = resolve_receiver_type(analysis, tok.line, tok.col) {
            let is_field = match &ty {
                Type::Named(n) => analysis
                    .record_fields
                    .iter()
                    .any(|(tn, c)| tn == n && c.label == tok.text),
                Type::Record(fields) => fields.iter().any(|f| f.name == tok.text),
                _ => false,
            };
            if is_field {
                return Some((SemKind::Property, SemMods::default()));
            }
        }
    }

    // 3. The namespace binding itself (`ns` in `import * as ns` / `ns.member`).
    if analysis.namespaces.iter().any(|n| n.name == tok.text) {
        return Some((SemKind::Namespace, SemMods::default()));
    }

    // 4. Top-level symbols (open document wins over an imported same-name;
    //    latest declaration wins) — this is where import specifiers get their
    //    real kind, since imported decls are indexed with their source file.
    if let Some(best) = analysis
        .symbols
        .iter()
        .filter(|s| s.name == tok.text)
        .max_by_key(|s| (s.file.is_none(), s.line))
    {
        let declaration = best.file.is_none() && best.line == tok.line && best.col == tok.col;
        let readonly = best.kind == SymbolKind::Global
            && best.detail.starts_with("let ")
            && !best.detail.starts_with("let mut");
        return Some((
            sem_of_symbol_kind(best.kind),
            SemMods { declaration, readonly, default_library: is_std_file(&best.file), last_use: false },
        ));
    }

    // 5. Compiler builtins: free-function intrinsics → macro; option/result
    //    constructors → enumMember; method builtins (`push`, `info`) → method.
    if MACRO_BUILTINS.contains(&tok.text.as_str()) {
        return Some((SemKind::Macro, mods_default_lib()));
    }
    if CONSTRUCTOR_BUILTINS.contains(&tok.text.as_str()) {
        return Some((SemKind::EnumMember, mods_default_lib()));
    }
    if builtin_method(&tok.text).is_some() {
        return Some((SemKind::Method, mods_default_lib()));
    }
    None
}

/// `SemMods` with only `default_library` set (the common builtin shape).
fn mods_default_lib() -> SemMods {
    SemMods { declaration: false, readonly: false, default_library: true, last_use: false }
}

// ---------------------------------------------------------------------------
// built-in methods (method-call resolution + `.foo` member completion)
// ---------------------------------------------------------------------------

/// A built-in method/function — one of the reserved call names the checker
/// handles inline in `call()` (e.g. `Array.push`, `Logger.info`, `Ref.get`).
/// These have no source declaration, so they synthesize hover text only (no
/// go-to-definition target). Just two `&'static str`s, so `Copy` is free.
#[derive(Clone, Copy)]
struct BuiltinMethod {
    name: &'static str,
    detail: &'static str,
}

/// The table of built-in call names that are valid as a method on *some*
/// receiver (their arity/receiver-type check is done by the checker, not here).
/// Looked up by name for hover on a bare method-name token. Order is irrelevant
/// for lookup; it only matters that each name maps to one entry.
static ALL_BUILTIN_METHODS: &[BuiltinMethod] = &[
    BuiltinMethod { name: "push", detail: "push(array, value) -> Array<T> — append to a growable array" },
    BuiltinMethod { name: "at", detail: "at(array, index) -> T — read an element by index" },
    BuiltinMethod { name: "alen", detail: "alen(array) -> Int64 — element count" },
    // RFC-0075. `close` is offered on a `Stream<T>` below; `fromArray` is here
    // for hover only, since it takes an Array and produces the stream.
    BuiltinMethod { name: "fromArray", detail: "fromArray(array) -> Stream<T> — move an array's elements into a linear stream" },
    BuiltinMethod { name: "fromStep", detail: "fromStep(slot, generation, step) -> Stream<T> — a stream that pulls from `step: fn(Int64, Int64, Bool) -> Option<T>` over a cursor its caller minted; `std/stream`'s `unfold` is the one to call" },
    BuiltinMethod { name: "boxStream", detail: "boxStream(s) -> Int64 — move a stream into one heap box and answer its address; `std/stream` keeps a wrapper's source this way (RFC-0090 M3)" },
    BuiltinMethod { name: "unboxStream", detail: "unboxStream(address) -> Stream<T> — take a boxed stream back out; needs its type from the annotation: `let s: Stream<T> = unboxStream(a)`" },
    BuiltinMethod { name: "pullAt", detail: "pullAt(address) -> Option<T> — one element from the stream in that box; needs its type from the annotation: `let x: Option<T> = pullAt(a)`" },
    BuiltinMethod { name: "close", detail: "close(stream) -> Unit — discharge a stream's disposal obligation without consuming it" },
    BuiltinMethod { name: "serveStream", detail: "serveStream(stream) -> Unit — hand a `Stream<String>` of encoded frames to the serving host, which writes each one and closes the stream the first time a write fails (RFC-0074 M3a); `std/http`'s `sse` is the one to call" },
    BuiltinMethod { name: "pop", detail: "array.pop() -> Option<T> — remove and return the last element (None if empty)" },
    BuiltinMethod { name: "swapRemove", detail: "array.swapRemove(index) -> T — O(1) unordered remove: move the last element into the slot" },
    BuiltinMethod { name: "has", detail: "map.has(key) -> Bool — whether the map contains the key (RFC-0028)" },
    BuiltinMethod { name: "remove", detail: "map.remove(key) -> Bool — remove the entry (order-preserving); was it present? (RFC-0028)" },
    BuiltinMethod { name: "keys", detail: "map.keys() -> Array<String> — a snapshot of the keys, in insertion order (RFC-0028)" },
    BuiltinMethod { name: "toArray", detail: "smallArray.toArray() -> Array<T> — copy a SmallArray's elements out to a growable Array (RFC-0056)" },
    BuiltinMethod { name: "copy", detail: "x.copy() -> T — a value of the receiver's type that shares no heap with it; deep and structural (RFC-0089). A handle copies as the value it is, so the copy names the same thing" },
    BuiltinMethod { name: "toString", detail: "x.toString() -> String — render a number, Bool, or String" },
    BuiltinMethod { name: "charCount", detail: "s.charCount() -> Int64 — number of Unicode scalar values (O(n); counts non-continuation bytes)" },
    BuiltinMethod { name: "join", detail: "task.join() -> T — await a spawned task's result" },
    BuiltinMethod { name: "trace", detail: "trace(logger, message) -> Unit — log at trace level" },
    BuiltinMethod { name: "debug", detail: "debug(logger, message) -> Unit — log at debug level" },
    BuiltinMethod { name: "info", detail: "info(logger, message) -> Unit — log at info level" },
    BuiltinMethod { name: "warn", detail: "warn(logger, message) -> Unit — log at warn level" },
    BuiltinMethod { name: "error", detail: "error(logger, message) -> Unit — log at error level" },
];

/// Hover text for a built-in call name, if `name` is one. Used by [`resolve`] as
/// the fallback when no user symbol or local matches.
fn builtin_method(name: &str) -> Option<&'static BuiltinMethod> {
    ALL_BUILTIN_METHODS.iter().find(|b| b.name == name)
}

/// The built-in methods valid on a receiver of type `ty`. Used by
/// [`member_completions`] to list members after a `.`. Mirrors the receiver-type
/// dispatch in the checker's `call()` — the builtins are grouped by what base
/// type they operate on.
fn builtin_methods_for(ty: &Type) -> Vec<BuiltinMethod> {
    let by_name = |n: &str| ALL_BUILTIN_METHODS.iter().find(|b| b.name == n).copied();
    let mut out = builtin_methods_of_shape(ty);
    // `x.copy()` (RFC-0089 M1b) is legal on every receiver, and offered on the
    // ones where it does something: a value that owns heap by its shape alone.
    // On a scalar it is the identity, and a completion list is a place to say
    // what is worth writing.
    if matches!(
        ty,
        Type::Str
            | Type::Array(_)
            | Type::ArrayN(..)
            | Type::SmallArray(..)
            | Type::Map(..)
            | Type::Record(_)
            | Type::Option(_)
            | Type::Result(..)
    ) {
        out.extend(by_name("copy"));
    }
    out
}

fn builtin_methods_of_shape(ty: &Type) -> Vec<BuiltinMethod> {
    let by_name = |n: &str| ALL_BUILTIN_METHODS.iter().find(|b| b.name == n).copied();
    match ty {
        // A growable `Array<T>` offers the full mutation surface, including the
        // shrinking ops `pop`/`swapRemove` (RFC-0011).
        Type::Array(_) => vec![
            by_name("push"),
            by_name("at"),
            by_name("alen"),
            by_name("pop"),
            by_name("swapRemove"),
        ]
        .into_iter()
        .flatten()
        .collect(),
        // A `Stream<T>` (RFC-0075) offers exactly one method, which is the point:
        // no `at`, no `alen`, no `push`. It is read by `for … in` or released.
        Type::Stream(_) => vec![by_name("close")].into_iter().flatten().collect(),
        // A fixed-size `Array<T, N>` cannot shrink — no `pop`/`swapRemove`.
        Type::ArrayN(..) => vec![by_name("push"), by_name("at"), by_name("alen")]
            .into_iter()
            .flatten()
            .collect(),
        // A `SmallArray<T, N>` (RFC-0056) is API-identical to a growable
        // `Array<T>` — the full mutation surface — plus `toArray` (copy-out).
        Type::SmallArray(..) => vec![
            by_name("push"),
            by_name("at"),
            by_name("alen"),
            by_name("pop"),
            by_name("swapRemove"),
            by_name("toArray"),
        ]
        .into_iter()
        .flatten()
        .collect(),
        // A `Map<String, V>` (RFC-0028): `has`/`remove`/`keys` methods plus the
        // `.length` field (surfaced by field completion, like a String's length).
        Type::Map(..) => vec![by_name("has"), by_name("remove"), by_name("keys")]
            .into_iter()
            .flatten()
            .collect(),
        // `String` offers `.toString()` and `.charCount()` (RFC-0058); its
        // `.byteLength` field is surfaced by field completion, not here.
        Type::Str => vec![by_name("toString"), by_name("charCount")]
            .into_iter()
            .flatten()
            .collect(),
        Type::Int | Type::IntN { .. } | Type::Float | Type::Float32 | Type::Bool => {
            vec![by_name("toString")].into_iter().flatten().collect()
        }
        Type::Logger => vec![
            by_name("trace"),
            by_name("debug"),
            by_name("info"),
            by_name("warn"),
            by_name("error"),
        ]
        .into_iter()
        .flatten()
        .collect(),
        Type::Task(_) => vec![by_name("join")].into_iter().flatten().collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC-0042: a sequence (`Tw`) validated string type's alphabet is enumerated
    /// into `sequence_string_types`; `class_completions` offers it at a `cls("…")`
    /// argument; `class_token_hover` shows the utility's CSS or "safelisted".
    #[test]
    fn rfc42_tw_alphabet_completion_and_hover() {
        let src = "type TwClass = String where value =~ \"(a-1|a-2|flex)\"\n\
                   type Tw = String where value =~ \"(a-1|a-2|flex)( (a-1|a-2|flex))*\"\n\
                   fn cls(c: Tw) -> Int64 { return 0 }\n\
                   fn css() -> String { return \".a-1 {color:red}\\n.flex {display:flex}\" }\n\
                   fn main() -> Int64 { return cls(\"flex a-1\") }\n";
        let a = analyze(src);
        // The alphabet is enumerated (order-independent set check).
        let (_, alpha) = a.sequence_string_types.iter().find(|(n, _)| n == "Tw").expect("Tw seq");
        for m in ["a-1", "a-2", "flex"] {
            assert!(alpha.iter().any(|s| s == m), "alphabet has {m}: {alpha:?}");
        }
        // A finite type is NOT a sequence type (whole-domain path owns those).
        assert!(!a.sequence_string_types.iter().any(|(n, _)| n == "TwClass"));

        // `return cls("flex a-1")` is on line 5; the string opens at the `"`.
        let line = 5;
        let col = src.lines().nth(4).unwrap().find("flex a-1").unwrap() + 1 + 1; // inside 'flex'
        let items = class_completions(&a, src, line, col).expect("class completions");
        assert!(items.iter().any(|c| c.label == "flex"));
        assert!(items.iter().any(|c| c.label == "a-2"));

        // Hover on the `flex` token → its CSS rule; on `a-2` (no rule) → safelisted.
        let hv = class_token_hover(&a, src, line, col).expect("hover on class token");
        assert!(hv.contains("display:flex"), "utility CSS: {hv}");
        let col_a1 = src.lines().nth(4).unwrap().find("a-1\")").unwrap() + 1 + 1;
        let hv2 = class_token_hover(&a, src, line, col_a1).expect("hover a-1");
        assert!(hv2.contains("color:red"), "a-1 rule: {hv2}");
    }

    /// `css_rule_for` extracts base and variant rules and resists prefix
    /// collisions (`.p-2` must not be satisfied by `.p-20`).
    #[test]
    fn rfc42_css_rule_lookup() {
        let css = ".p-2 {padding:0.5rem}\n.p-20 {padding:5rem}\n\
                   .md\\:hover\\:bg-x:hover {background:#000}";
        assert_eq!(css_rule_for(css, "p-2").as_deref(), Some(".p-2 {padding:0.5rem}"));
        assert_eq!(css_rule_for(css, "p-20").as_deref(), Some(".p-20 {padding:5rem}"));
        assert_eq!(
            css_rule_for(css, "md:hover:bg-x").as_deref(),
            Some(".md\\:hover\\:bg-x:hover {background:#000}")
        );
        assert_eq!(css_rule_for(css, "missing"), None);
    }

    #[test]
    fn module_state_is_indexed_with_hover_detail() {
        // RFC-0013: globals appear in the symbol index (hover / go-to-def /
        // completion). The annotated one shows its type; the unannotated one
        // infers from its literal initializer.
        let src = "let mut hits = 0\n\
                   let banner: String = \"hi\"\n\
                   fn main() -> Int64 { return hits }";
        let a = analyze(src);
        let hits = a
            .symbols
            .iter()
            .find(|s| s.name == "hits")
            .expect("hits symbol");
        assert_eq!(hits.kind, SymbolKind::Global);
        assert_eq!(hits.detail, "let mut hits: Int64");
        assert_eq!(hits.line, 1);
        assert!(hits.col > 0, "has a name column for go-to-def");

        let banner = a
            .symbols
            .iter()
            .find(|s| s.name == "banner")
            .expect("banner symbol");
        assert_eq!(banner.kind, SymbolKind::Global);
        assert_eq!(banner.detail, "let banner: String");
    }

    #[test]
    fn tests_appear_in_the_symbol_index() {
        // RFC-0015: each `test "name"` block is in the outline as a Method with a
        // `test "name"` detail, anchored on its declaration line.
        let src = "test \"adds up\" { assert(1 + 1 == 2) }\n\
                   fn main() -> Int64 { return 0 }";
        let a = analyze(src);
        let t = a
            .symbols
            .iter()
            .find(|s| s.name == "adds up")
            .expect("test symbol");
        assert_eq!(t.kind, SymbolKind::Method);
        assert_eq!(t.detail, "test \"adds up\"");
        assert_eq!(t.line, 1);
        assert!(t.col > 0, "anchored at the `test` keyword for go-to");
    }

    #[test]
    fn benches_appear_in_the_symbol_index() {
        // RFC-0055: each `bench "name"` block is in the outline as a Method with a
        // `bench "name"` detail, anchored on its declaration line (like tests).
        let src = "bench \"hot path\" { blackBox(1) }\n\
                   fn main() -> Int64 { return 0 }";
        let a = analyze(src);
        let b = a
            .symbols
            .iter()
            .find(|s| s.name == "hot path")
            .expect("bench symbol");
        assert_eq!(b.kind, SymbolKind::Method);
        assert_eq!(b.detail, "bench \"hot path\"");
        assert_eq!(b.line, 1);
        assert!(b.col > 0, "anchored at the `bench` keyword for go-to");
    }

    #[test]
    fn analyze_linked_runs_a_generator_import() {
        // RFC-0021: editor analysis resolves a generator-call import through the
        // loader — the generator runs, its module links, and the imported name is
        // indexed for hover / go-to-def (via the read-only resolver + cache).
        use crate::loader::{LoadOptions, MapResolver};
        let files: std::collections::HashMap<String, String> = [(
            "gen.vyrn".to_string(),
            "export gen fn mk(d: String) -> String { \
                 return \"export fn magic() -> Int64 { return 7 }\" }"
                .to_string(),
        )]
        .into_iter()
        .collect();
        let resolver = MapResolver(files);
        let root = "import { mk } from \"./gen\"\n\
                    import { magic } from mk(\"./data\")\n\
                    fn main() -> Int64 { return magic() }";
        let a = analyze_linked(root, "main.vyrn", &LoadOptions::default(), &resolver);
        assert!(
            a.diagnostics.is_empty(),
            "diags: {:?}",
            a.diagnostics
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
        );
        assert!(
            a.symbols.iter().any(|s| s.name == "magic"),
            "generated `magic` is indexed"
        );
    }

    #[test]
    fn analyze_linked_indexes_an_aliased_import() {
        // RFC-0022: an aliased import is indexed under the LOCAL name, hover notes
        // `— alias of <original>`, and go-to-def points at the foreign decl.
        use crate::loader::{LoadOptions, MapResolver};
        let files: std::collections::HashMap<String, String> = [(
            "api.vyrn".to_string(),
            "export fn getUser(id: Int64) -> Int64 { return id }".to_string(),
        )]
        .into_iter()
        .collect();
        let resolver = MapResolver(files);
        let root = "import { getUser as fetchUser } from \"./api\"\n\
                    fn main() -> Int64 { return fetchUser(1) }";
        let a = analyze_linked(root, "main.vyrn", &LoadOptions::default(), &resolver);
        assert!(a.diagnostics.is_empty(), "diags: {:?}", a.diagnostics);
        let sym = a
            .symbols
            .iter()
            .find(|s| s.name == "fetchUser" && s.file.is_some())
            .expect("aliased import indexed under the local name");
        assert!(
            sym.detail.contains("alias of `getUser`"),
            "hover detail: {}",
            sym.detail
        );
        assert_eq!(
            sym.file.as_deref(),
            Some("api.vyrn"),
            "go-to-def jumps to the source module"
        );
        // The original name is NOT indexed as an imported symbol.
        assert!(
            !a.symbols
                .iter()
                .any(|s| s.name == "getUser" && s.file.is_some()),
            "original name is hidden by the alias"
        );
    }

    #[test]
    fn analyze_linked_indexes_namespace_members() {
        // RFC-0027: `import * as ns` indexes the module's exports for `ns.`
        // completion and `ns.member` hover / cross-file go-to-definition.
        use crate::loader::{LoadOptions, MapResolver};
        let files: std::collections::HashMap<String, String> = [(
            "api.vyrn".to_string(),
            "export type User = { id: Int64 }\n\
             export fn getUser(id: Int64) -> User { return User { id: id } }"
                .to_string(),
        )]
        .into_iter()
        .collect();
        let resolver = MapResolver(files);
        let root = "import * as api from \"./api\"\n\
                    fn main() -> Int64 { let u = api.getUser(1) return u.id }";
        let a = analyze_linked(root, "main.vyrn", &LoadOptions::default(), &resolver);
        assert!(a.diagnostics.is_empty(), "diags: {:?}", a.diagnostics);

        // The namespace binding and its members are recorded.
        let nsi = a
            .namespaces
            .iter()
            .find(|n| n.name == "api")
            .expect("namespace `api` indexed");
        assert!(
            nsi.members.iter().any(|m| m.name == "getUser"),
            "getUser member"
        );
        assert!(nsi.members.iter().any(|m| m.name == "User"), "User member");

        // Columns are 1-based; line 2 is the `fn main` body.
        let body = root.lines().nth(1).unwrap();
        let getuser_col = body.find("getUser").unwrap() + 1;

        // Completion at the `getUser` member position offers the module's exports.
        let comps = member_completions(&a, 2, getuser_col);
        assert!(
            comps.iter().any(|c| c.label == "getUser"),
            "completions: {comps:?}"
        );
        assert!(
            comps
                .iter()
                .all(|c| c.detail.contains("via namespace `api`")),
            "via-namespace note"
        );

        // Go-to-definition on the `getUser` in `api.getUser` jumps into api.vyrn.
        let r = resolve(&a, 2, getuser_col).expect("resolve api.getUser");
        assert_eq!(r.name, "getUser");
        assert_eq!(
            r.target_file.as_deref(),
            Some("api.vyrn"),
            "cross-file go-to-def"
        );
        assert!(
            r.hover.contains("via namespace `api`"),
            "hover note: {}",
            r.hover
        );

        // Hovering the `api` binding shows the namespace hover (not a value).
        let acol = body.find("api.").unwrap() + 1;
        let rn = resolve(&a, 2, acol).expect("resolve namespace name");
        assert!(
            rn.hover.contains("namespace `api`"),
            "namespace hover: {}",
            rn.hover
        );
    }

    // ---- RFC-0028: Map method completion ------------------------------------

    #[test]
    fn map_receiver_completes_its_method_surface() {
        // `.` on a Map-typed local offers `has`/`remove`/`keys` and `length`.
        let src = "fn main() -> Int64 {\n\
                   let mut m: Map<String, Int64> = [:]\n\
                   let x = m.has(\"a\")\n\
                   return 0 }";
        let a = analyze(src);
        let line = src.lines().nth(2).unwrap();
        // Cursor just after `m.` (1-based column).
        let col = line.find("m.").unwrap() + 3;
        let comps = member_completions(&a, 3, col);
        let labels: Vec<&str> = comps.iter().map(|c| c.label.as_str()).collect();
        for want in ["has", "remove", "keys", "length"] {
            assert!(labels.contains(&want), "expected `{want}` in {labels:?}");
        }
        // Array-only shrinking ops are NOT offered on a Map.
        assert!(
            !labels.contains(&"pop"),
            "map must not offer `pop`: {labels:?}"
        );
    }

    /// RFC-0089 M1b: `copy` completes on a receiver that owns heap, and hovers
    /// with the rule rather than with a signature alone.
    #[test]
    fn copy_completes_and_hovers_on_an_owning_receiver() {
        let src = "fn main() -> Int64 {\n\
                   let s = \"a\" + \"b\"\n\
                   let t = s.copy()\n\
                   return t.byteLength }";
        let a = analyze(src);
        let line = src.lines().nth(2).unwrap();
        let col = line.find("s.").unwrap() + 3;
        let labels: Vec<String> =
            member_completions(&a, 3, col).into_iter().map(|c| c.label).collect();
        assert!(labels.iter().any(|l| l == "copy"), "{labels:?}");
        let hover = builtin_method("copy").expect("`copy` hovers").detail;
        assert!(hover.contains("shares no heap"), "{hover}");
        // A scalar receiver does not offer it: there it is the identity.
        let scalar = "fn main() -> Int64 {\n\
                      let n = 5\n\
                      let m = n.toString()\n\
                      return 0 }";
        let a2 = analyze(scalar);
        let l2 = scalar.lines().nth(2).unwrap();
        let c2 = l2.find("n.").unwrap() + 3;
        let labels2: Vec<String> =
            member_completions(&a2, 3, c2).into_iter().map(|c| c.label).collect();
        assert!(!labels2.iter().any(|l| l == "copy"), "{labels2:?}");
    }

    // ---- RFC-0020 M1: string-literal completion -----------------------------

    const TRANSKEY: &str =
        "type TransKey = String where value =~ \"nav\\\\.(home|about)\\\\.label\"\n";

    #[test]
    fn finite_string_type_is_enumerated_in_analysis() {
        let a = analyze(&format!("{TRANSKEY}fn main() -> Int64 {{ return 0 }}"));
        let (_, domain) = a
            .finite_string_types
            .iter()
            .find(|(n, _)| n == "TransKey")
            .expect("TransKey enumerated");
        assert_eq!(
            domain,
            &vec!["nav.about.label".to_string(), "nav.home.label".to_string()]
        );
    }

    #[test]
    fn string_literal_completion_at_a_call_argument() {
        let src = format!(
            "{TRANSKEY}fn t(key: TransKey) -> Int64 {{ return 0 }}\n\
             fn main() -> Int64 {{ return t(\"\") }}"
        );
        // The `""` opens at col 31 on line 3; cursor inside at col 32.
        let items = string_literal_completions(&analyze(&src), &src, 3, 32);
        let labels: Vec<&str> = items.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, vec!["nav.about.label", "nav.home.label"]);
    }

    #[test]
    fn string_literal_completion_at_an_annotated_let() {
        let src = format!("{TRANSKEY}fn main() -> Int64 {{ let k: TransKey = \"\"  return 0 }}");
        // Line 2: `... let k: TransKey = ""  ...` — the `""` opens at col 40.
        let items = string_literal_completions(&analyze(&src), &src, 2, 41);
        let labels: Vec<&str> = items.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, vec!["nav.about.label", "nav.home.label"]);
    }

    #[test]
    fn over_cap_or_infinite_type_offers_nothing() {
        // An infinite regex string type is never enumerated, so a literal at its
        // argument position offers no completions.
        let src = "type Any = String where value =~ \"[a-z]+\"\n\
                   fn f(x: Any) -> Int64 { return 0 }\n\
                   fn main() -> Int64 { return f(\"\") }";
        let a = analyze(src);
        assert!(a.finite_string_types.iter().all(|(n, _)| n != "Any"));
        assert!(string_literal_completions(&a, src, 3, 32).is_empty());
    }

    #[test]
    fn string_literal_completion_outside_a_typed_context_is_empty() {
        // A plain-String argument has no finite domain → nothing to offer.
        let src = format!(
            "{TRANSKEY}fn g(s: String) -> Int64 {{ return 0 }}\n\
             fn main() -> Int64 {{ return g(\"\") }}"
        );
        assert!(string_literal_completions(&analyze(&src), &src, 3, 32).is_empty());
    }

    // RFC-0065: `module_doc` — the doc model behind `vyrn doc`.

    #[test]
    fn module_doc_lists_only_exports_in_declaration_order() {
        let src = "export fn b() -> Int64 { return 0 }\n\
                   fn priv() -> Int64 { return 0 }\n\
                   export type A = { x: Int64 }\n";
        let d = module_doc(src);
        let names: Vec<&str> = d.exports.iter().map(|e| e.name.as_str()).collect();
        // `priv` omitted (not exported); order follows source lines.
        assert_eq!(names, ["b", "A"]);
        assert_eq!(d.exports[0].signature, "fn b() -> Int64");
        assert_eq!(d.exports[1].signature, "type A = { x: Int64 }");
    }

    #[test]
    fn a_mut_fn_shows_its_marker_in_the_signature() {
        // RFC-0074 M4a: the marker decides whether a projection calls the
        // procedure a mutation, so hover and `vyrn doc` have to show it.
        let src = "export mut fn create(x: Int64) -> Int64 { return x }\n\
                   export fn read() -> Int64 { return 0 }\n";
        let d = module_doc(src);
        assert_eq!(d.exports[0].signature, "mut fn create(x: Int64) -> Int64");
        assert_eq!(d.exports[1].signature, "fn read() -> Int64");
    }

    #[test]
    fn module_doc_attaches_a_declaration_doc_but_not_a_detached_one() {
        // A `///` directly above a decl attaches; one split off by a blank line
        // does not (the established detach rule) — so `area` carries no doc.
        let src = "/// attaches to foo\n\
                   export fn foo() -> Int64 { return 0 }\n\
                   \n\
                   /// detached from area by a blank line\n\
                   \n\
                   export fn area() -> Int64 { return 0 }\n";
        let d = module_doc(src);
        assert_eq!(d.exports[0].doc.as_deref(), Some("attaches to foo"));
        assert_eq!(d.exports[1].doc, None);
    }

    #[test]
    fn module_doc_carries_documented_protocol_methods_only() {
        let src = "/// the protocol\n\
                   export protocol P {\n\
                   /// what show does\n\
                   fn show(self) -> String\n\
                   fn debug(self) -> String\n\
                   }\n";
        let d = module_doc(src);
        assert_eq!(d.exports[0].doc.as_deref(), Some("the protocol"));
        assert_eq!(
            d.exports[0].members,
            vec![("fn show(self) -> String".to_string(), "what show does".to_string())]
        );
    }

    /// The same `///` reaches hover: the symbol a cursor on `show` resolves to
    /// carries the method's doc, exactly as a free function's does.
    #[test]
    fn a_protocol_method_symbol_carries_its_doc() {
        let src = "export protocol P {\n\
                   /// what show does\n\
                   fn show(self) -> String\n\
                   }\n";
        let a = analyze(src);
        let m = a.symbols.iter().find(|s| s.name == "show").unwrap();
        assert_eq!(m.kind, SymbolKind::Method);
        assert_eq!(m.doc.as_deref(), Some("what show does"));
    }

    /// Hovering the CALL site (`r.show()`) is what the defect was actually
    /// about. It resolves through the impl, whose body carries no `///` of its
    /// own — the protocol signature's doc is the one that describes it.
    #[test]
    fn hovering_a_method_call_shows_the_protocol_signatures_doc() {
        let src = "type R = { x: Int64 }\n\
protocol P {\n\
/// what show does\n\
fn show(self) -> String\n\
}\n\
impl P for R {\n\
fn show(self) -> String { return \"r\" }\n\
}\n\
fn main() -> Int64 {\n\
let r = R { x: 1 }\n\
let s = r.show()\n\
return 0\n\
}\n";
        let a = analyze(src);
        let r = resolve(&a, 11, 11).expect("show at (11,11) should resolve");
        assert_eq!(r.hover, "fn show(self: R) -> String\n\nwhat show does");
        // Go-to-definition still lands on the impl, not the signature.
        assert_eq!(r.target_line, 7);
    }

    #[test]
    fn module_doc_header_is_the_detached_leading_block() {
        // A leading block split from the first decl by a blank line is the file
        // header; without the gap it belongs to the declaration instead.
        let detached = "/// file header\n\
                        /// second line\n\
                        \n\
                        export fn f() -> Int64 { return 0 }\n";
        let d = module_doc(detached);
        assert_eq!(d.header_doc.as_deref(), Some("file header\nsecond line"));
        assert_eq!(d.exports[0].doc, None);

        let attached = "/// belongs to f\n\
                        export fn f() -> Int64 { return 0 }\n";
        let d = module_doc(attached);
        assert_eq!(d.header_doc, None);
        assert_eq!(d.exports[0].doc.as_deref(), Some("belongs to f"));
    }

    #[test]
    fn module_doc_preserves_a_fenced_block_verbatim() {
        let src = "/// A diagram.\n\
                   ///\n\
                   /// ```mermaid\n\
                   /// flowchart LR\n\
                   ///   a --> b\n\
                   /// ```\n\
                   export fn f() -> Int64 { return 0 }\n";
        let d = module_doc(src);
        assert_eq!(
            d.exports[0].doc.as_deref(),
            Some("A diagram.\n\n```mermaid\nflowchart LR\n  a --> b\n```")
        );
    }

    // -----------------------------------------------------------------------
    // RFC-0087 U1 — the memory model, in the editor
    // -----------------------------------------------------------------------

    /// One body with all four outcomes a binding can have.
    const MEM_SRC: &str = r#"fn take(s: consume String) -> Int64 { return 1 }
fn main() -> Int64 {
    let a = "x" + "y"
    let n = take(a)
    let b = "p" + "q"
    let c = b.copy()
    drop c
    print(b)
    return n
}
"#;

    #[test]
    fn memory_notes_say_what_happens_to_each_binding() {
        let a = analyze(MEM_SRC);
        assert!(a.diagnostics.is_empty(), "{:?}", a.diagnostics);
        let note = |n: &str| {
            a.memory.iter().find(|m| m.name == n).map(|m| m.text.clone()).unwrap_or_default()
        };
        assert_eq!(note("a"), "moved at line 4 into `take(..)`");
        assert_eq!(note("b"), "reclaimed at block exit — freeing the String buffer");
        assert_eq!(note("c"), "reclaimed by `drop` at line 7");
        // `n` is an Int64. There is nothing to reclaim, so there is no sentence
        // to say about it — a "NOT reclaimed" hover on a scalar is only alarm.
        assert!(a.memory.iter().all(|m| m.name != "n"), "{:?}", a.memory);
    }

    #[test]
    fn a_binding_hover_carries_its_memory_answer() {
        let a = analyze(MEM_SRC);
        // The `a` in `let a = ..` on line 3.
        let r = resolve(&a, 3, 9).expect("a resolves");
        assert!(r.hover.contains("memory: moved at line 4 into `take(..)`"), "{}", r.hover);
    }

    #[test]
    fn a_move_gets_an_inlay_hint_where_the_value_goes() {
        let a = analyze(MEM_SRC);
        let hints = inlay_hints(&a);
        assert_eq!(hints.len(), 1, "{hints:?}");
        assert_eq!(hints[0].line, 4);
        assert_eq!(hints[0].label, "→ take(..)");
    }

    #[test]
    fn the_last_use_of_an_owning_binding_is_marked() {
        let a = analyze(MEM_SRC);
        let marked: Vec<(usize, usize)> = semantic_tokens(&a)
            .into_iter()
            .filter(|t| t.mods.last_use)
            .map(|t| (t.line, t.col))
            .collect();
        // The `a` handed to `take` (line 4) and the `c` a `drop` takes (line 7).
        // The declaration is never the last use, and `b` has no such point —
        // it lives to block exit.
        assert_eq!(marked.len(), 2, "{marked:?}");
        assert_eq!(marked[0].0, 4);
        assert_eq!(marked[1].0, 7);
    }

    #[test]
    fn a_document_that_does_not_check_gets_no_memory_answer() {
        // An answer about a body the checker refused is an answer about a body
        // nobody will run, and computing one is what would put an extra walk on
        // the keystroke path for the buffers that need it least.
        let a = analyze("fn main() -> Int64 { let s = nope() return 0 }");
        assert!(!a.diagnostics.is_empty());
        assert!(a.memory.is_empty());
    }
}
