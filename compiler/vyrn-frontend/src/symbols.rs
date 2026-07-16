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
    self, Block, EnumVariant, Expr, Function, GlobalDecl, MethodSig, ProtocolDecl, Stmt, Type,
    TypeDecl,
};
use crate::checker;
use crate::diagnostics::Diagnostic;
use crate::lexer::{self, Tok};
use crate::movecheck;
use crate::parser;

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
    /// RFC-0027: each `import * as ns` binding and the exported symbols reachable
    /// through it, so `ns.` completes and `ns.member` hovers / jumps into the
    /// source module. Populated only by [`analyze_linked`].
    pub namespaces: Vec<NamespaceInfo>,
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
    linker: Option<(&str, &crate::loader::LoadOptions, &dyn crate::loader::ModuleResolver)>,
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
    let checked: Option<crate::ast::Program> = if parse_failed {
        None
    } else {
        match (&linker, program.imports.is_empty()) {
            (Some((root_path, opts, resolver)), false) => {
                match crate::loader::load(source, root_path, opts, *resolver) {
                    Ok(linked) => Some(linked),
                    Err(load_diags) => {
                        diags.extend(load_diags.into_iter().map(adopt_foreign));
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
            let (check_diags, let_types) = checker::check_accum_with_let_types(prog);
            diags.extend(check_diags.into_iter().map(adopt_foreign));
            diags.extend(movecheck::check_accum(prog).into_iter().map(adopt_foreign));
            let_types
        }
        None => Default::default(),
    };
    pin_diagnostics(&mut diags, &kw_cols, &tok_info);

    let decl_lines = decl_lines(&program);
    let fn_lines = fn_lines(&program);
    let mut symbols = index_symbols(&program, &tok_info, &decl_lines);
    // Cross-file symbols: declarations the root imports, indexed from the
    // linked program with their source file, so hover shows the signature and
    // go-to-definition jumps into the imported module. A no-op without a
    // linker (a plain-`analyze` program has no `module`-tagged decls).
    if let Some(linked) = &checked {
        symbols.extend(index_imported_symbols(&program, linked));
    }
    // RFC-0027: namespace bindings and their reachable exports (for `ns.`
    // completion and `ns.member` hover / go-to-definition). Needs the linker to
    // resolve each namespace import to its source module.
    let namespaces = index_namespaces(source, &program, linker);
    let locals = index_locals(&program, &tok_info, &let_types);

    // Protocol/impl member tables for `.foo` completion (RFC-0002 §5). Impls
    // and protocols come from the linked program when available (imported
    // impls count — coherence makes them global); bounds come from the root's
    // functions (only root bodies have a cursor).
    let member_src = checked.as_ref().unwrap_or(&program);
    let mut impl_members = Vec::new();
    for imp in &member_src.impls {
        for m in &imp.methods {
            impl_members.push((
                imp.ty.clone(),
                Completion {
                    label: m.name.clone(),
                    kind: SymbolKind::Method,
                    detail: function_detail(m),
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
                },
            ));
        }
    }
    let type_param_bounds = program
        .functions
        .iter()
        .chain(program.impls.iter().flat_map(|i| i.methods.iter()))
        .flat_map(|f| f.type_bounds.iter().map(|(tp, bs)| (f.line, tp.clone(), bs.clone())))
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
        .map(|f| (f.name.clone(), f.params.iter().map(|p| p.ty.clone()).collect()))
        .collect();

    Analysis {
        diagnostics: diags,
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
        namespaces,
    }
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
        namespaces: Vec::new(),
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
    kw_cols: &std::collections::HashMap<usize, std::collections::HashMap<&'static str, (usize, usize)>>,
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
            if let Some(t) = tok_info.iter().find(|t| t.line == d.line && t.text == target) {
                d.col = t.col;
                d.end_col = t.end_col;
                break;
            }
            // Keyword/operator path: `if`/`while`/`where`/`drop`/`match`/`?`/
            // `&&`/`||`/`!`. Reserved words are never identifiers, so the
            // identifier path above won't have matched them.
            if let Some(&(col, end_col)) =
                kw_cols.get(&d.line).and_then(|kws| kws.get(target))
            {
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
            return Some(local_resolution(b));
        }
    }

    // RFC-0027: `ns.member` — the member token, preceded by an in-scope (un-
    // shadowed) namespace, resolves to that module's export (hover + cross-file
    // go-to-definition). Checked before top-level symbols so a same-named local
    // decl doesn't capture a qualified member reference.
    if let Some(recv) = receiver_before_dot(analysis, tok.line, tok.col) {
        let shadowed = enclosing_fn_line(analysis, line).is_some_and(|fl| {
            analysis.locals.iter().any(|b| b.fn_line == fl && b.name == recv && b.line <= line)
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
                        hover: format!("{}\n\n— via namespace `{}`", m.detail, recv),
                        definition: m.file.is_some(),
                    });
                }
            }
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
            hover: best.detail.clone(),
            definition: true,
        });
    }

    // Last resort: a built-in method/function name (`push`, `info`, `len`, ...).
    // These have hover text but no source declaration, so go-to-definition has
    // nowhere to jump (`definition: false`).
    builtin_method(&tok.text).map(|b| Resolution {
        name: b.name.to_string(),
        kind: SymbolKind::Method,
        target_line: 0,
        target_col: 0,
        target_end_col: 0,
        target_file: None,
        hover: b.detail.to_string(),
        definition: false,
    })
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
    analysis.fn_lines.iter().any(|&l| l == seg_start).then_some(seg_start)
}

/// All top-level symbols as completion items. The client filters by the prefix
/// the user typed; v1 does no scope-aware filtering.
pub fn completions(analysis: &Analysis) -> Vec<Completion> {
    analysis
        .symbols
        .iter()
        .map(|s| Completion { label: s.name.clone(), kind: s.kind, detail: s.detail.clone() })
        .collect()
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
        })
        .collect();
    // `arr.length` is the element-count field sugar — a read-only field on
    // arrays, surfaced as a member alongside the array methods.
    if matches!(ty, Type::Array(_) | Type::ArrayN(..)) {
        out.push(Completion {
            label: "length".to_string(),
            kind: SymbolKind::Field,
            detail: "length: Int64 — element count (read-only)".to_string(),
        });
    }
    // `map.length` is the entry-count field sugar (RFC-0028).
    if matches!(ty, Type::Map(..)) {
        out.push(Completion {
            label: "length".to_string(),
            kind: SymbolKind::Field,
            detail: "length: Int64 — entry count (read-only)".to_string(),
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
    let Some((_, domain)) = analysis.finite_string_types.iter().find(|(n, _)| n == &ty_name) else {
        return Vec::new();
    };
    domain
        .iter()
        .map(|k| Completion {
            label: k.clone(),
            kind: SymbolKind::Variant,
            detail: format!("{ty_name} — a key of this finite string type"),
        })
        .collect()
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
    if str_idx >= 3
        && toks[str_idx - 1].tok == Tok::Eq
        && toks[str_idx - 3].tok == Tok::Colon
    {
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

fn index_symbols(
    program: &ast::Program,
    tok_info: &[TokenInfo],
    lines: &[usize],
) -> Vec<Symbol> {
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
            file: None,
        });
        if let Type::Enum(variants) = &t.base {
            // Variants carry no AST line; find the name token between this decl's
            // line and the next top-level declaration (or EOF).
            let until = lines.iter().find(|&&l| l > t.line).copied().unwrap_or(usize::MAX);
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
fn index_imported_symbols(root: &ast::Program, linked: &ast::Program) -> Vec<Symbol> {
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
                            file: Some(file.clone()),
                        });
                    }
                }
            }
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
    source: &str,
    root: &ast::Program,
    linker: Option<(&str, &crate::loader::LoadOptions, &dyn crate::loader::ModuleResolver)>,
) -> Vec<NamespaceInfo> {
    let Some((root_path, opts, resolver)) = linker else { return Vec::new() };
    if !root.imports.iter().any(|i| i.namespace.is_some()) {
        return Vec::new();
    }
    // The root module's resolved import targets, in `root.imports` order.
    let Ok(graph) = crate::loader::module_graph(source, root_path, opts, resolver) else {
        return Vec::new();
    };
    let root_key = crate::loader::normalize(root_path);
    let Some((_, targets)) = graph.iter().find(|(k, _)| *k == root_key) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (imp, target) in root.imports.iter().zip(targets) {
        if let Some(ns) = &imp.namespace {
            out.push(NamespaceInfo {
                name: ns.clone(),
                members: namespace_members(target, resolver),
            });
        }
    }
    out
}

/// The exported declarations of `target` as hover/goto-ready [`Symbol`]s. Parses
/// the target's own source (so names/lines are the module's, and a collision
/// rename in the linked program never leaks into what the editor shows). A target
/// that can't be read (a synthesized generator module has no file) yields no
/// members — completion after that `ns.` simply offers nothing.
fn namespace_members(target: &str, resolver: &dyn crate::loader::ModuleResolver) -> Vec<Symbol> {
    let Ok(text) = resolver.read(target) else { return Vec::new() };
    let Ok(tokens) = lexer::lex(&text) else { return Vec::new() };
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
                file: Some(target.to_string()),
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
                file: Some(target.to_string()),
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
            file: Some(target.to_string()),
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
                    file: Some(target.to_string()),
                });
            }
        }
    }
    out
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
            Stmt::Let { name, mutable, ty, line, .. } => {
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
            Stmt::ForIn { var, body, line, .. } => {
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
            Stmt::If { then_block, else_block, .. } => {
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
fn local_resolution(b: &LocalBinding) -> Resolution {
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
        hover: local_detail(b),
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
    let kw = if f.is_export_extern {
        "export extern fn"
    } else if f.is_extern {
        "extern fn"
    } else {
        "fn"
    };
    format!("{} {}{}({}) -> {}", kw, f.name, tp, params, type_to_string(&f.ret))
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
        Expr::Float(_) => Some(Type::Float),
        Expr::Bool(_) => Some(Type::Bool),
        Expr::Str(_) => Some(Type::Str),
        Expr::Unary { expr, .. } => infer_literal_type(expr),
        Expr::ArrayLit { elems, .. } => {
            elems.first().and_then(infer_literal_type).map(|t| Type::Array(Box::new(t)))
        }
        _ => None,
    }
}

fn method_sig_detail(m: &MethodSig) -> String {
    // MethodSig.params are types only (names are dropped by the parser); the
    // receiver `self` is implied and prepended.
    let params = m.params.iter().map(type_to_string).collect::<Vec<_>>().join(", ");
    let sig = if params.is_empty() { "self".to_string() } else { format!("self, {}", params) };
    format!("fn {}({}) -> {}", m.name, sig, type_to_string(&m.ret))
}

fn protocol_detail(p: &ProtocolDecl) -> String {
    let ms = p.methods.iter().map(method_sig_detail).collect::<Vec<_>>().join("; ");
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
            let fs =
                fields.iter().map(|f| field_detail(f, all)).collect::<Vec<_>>().join(", ");
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
            v.payload.iter().map(type_to_string).collect::<Vec<_>>().join(", ")
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
            v.payload.iter().map(type_to_string).collect::<Vec<_>>().join(", ")
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
    BuiltinMethod { name: "afree", detail: "afree(array) -> Unit — free a growable array" },
    BuiltinMethod { name: "pop", detail: "array.pop() -> Option<T> — remove and return the last element (None if empty)" },
    BuiltinMethod { name: "swapRemove", detail: "array.swapRemove(index) -> T — O(1) unordered remove: move the last element into the slot" },
    BuiltinMethod { name: "has", detail: "map.has(key) -> Bool — whether the map contains the key (RFC-0028)" },
    BuiltinMethod { name: "remove", detail: "map.remove(key) -> Bool — remove the entry (order-preserving); was it present? (RFC-0028)" },
    BuiltinMethod { name: "keys", detail: "map.keys() -> Array<String> — a snapshot of the keys, in insertion order (RFC-0028)" },
    BuiltinMethod { name: "get", detail: "get(ref) -> T — read through a generational reference" },
    BuiltinMethod { name: "set", detail: "set(ref, value) -> Unit — write through a generational reference" },
    BuiltinMethod { name: "release", detail: "release(ref) -> Unit — release a generational reference" },
    BuiltinMethod { name: "toString", detail: "x.toString() -> String — render a number, Bool, or String" },
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
    match ty {
        // A growable `Array<T>` offers the full mutation surface, including the
        // shrinking ops `pop`/`swapRemove` (RFC-0011).
        Type::Array(_) => vec![
            by_name("push"),
            by_name("at"),
            by_name("alen"),
            by_name("afree"),
            by_name("pop"),
            by_name("swapRemove"),
        ]
        .into_iter()
        .flatten()
        .collect(),
        // A fixed-size `Array<T, N>` cannot shrink — no `pop`/`swapRemove`.
        Type::ArrayN(..) => vec![by_name("push"), by_name("at"), by_name("alen"), by_name("afree")]
            .into_iter()
            .flatten()
            .collect(),
        // A `Map<String, V>` (RFC-0028): `has`/`remove`/`keys` methods plus the
        // `.length` field (surfaced by field completion, like a String's length).
        Type::Map(..) => vec![by_name("has"), by_name("remove"), by_name("keys")]
            .into_iter()
            .flatten()
            .collect(),
        Type::Ref(_) => vec![by_name("get"), by_name("set"), by_name("release")].into_iter().flatten().collect(),
        // `String` renders with `.toString()` (its byte length is the `.length`
        // field, surfaced by record/field completion, not here).
        Type::Str | Type::Int | Type::IntN { .. } | Type::Float | Type::Float32 | Type::Bool => {
            vec![by_name("toString")].into_iter().flatten().collect()
        }
        Type::Logger => vec![by_name("trace"), by_name("debug"), by_name("info"), by_name("warn"), by_name("error")]
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

    #[test]
    fn module_state_is_indexed_with_hover_detail() {
        // RFC-0013: globals appear in the symbol index (hover / go-to-def /
        // completion). The annotated one shows its type; the unannotated one
        // infers from its literal initializer.
        let src = "let mut hits = 0\n\
                   let banner: String = \"hi\"\n\
                   fn main() -> Int64 { return hits }";
        let a = analyze(src);
        let hits = a.symbols.iter().find(|s| s.name == "hits").expect("hits symbol");
        assert_eq!(hits.kind, SymbolKind::Global);
        assert_eq!(hits.detail, "let mut hits: Int64");
        assert_eq!(hits.line, 1);
        assert!(hits.col > 0, "has a name column for go-to-def");

        let banner = a.symbols.iter().find(|s| s.name == "banner").expect("banner symbol");
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
        let t = a.symbols.iter().find(|s| s.name == "adds up").expect("test symbol");
        assert_eq!(t.kind, SymbolKind::Method);
        assert_eq!(t.detail, "test \"adds up\"");
        assert_eq!(t.line, 1);
        assert!(t.col > 0, "anchored at the `test` keyword for go-to");
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
            a.diagnostics.iter().map(|d| d.message.clone()).collect::<Vec<_>>()
        );
        assert!(a.symbols.iter().any(|s| s.name == "magic"), "generated `magic` is indexed");
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
        assert!(sym.detail.contains("alias of `getUser`"), "hover detail: {}", sym.detail);
        assert_eq!(sym.file.as_deref(), Some("api.vyrn"), "go-to-def jumps to the source module");
        // The original name is NOT indexed as an imported symbol.
        assert!(
            !a.symbols.iter().any(|s| s.name == "getUser" && s.file.is_some()),
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
             export fn getUser(id: Int64) -> User { return User { id: id } }".to_string(),
        )]
        .into_iter()
        .collect();
        let resolver = MapResolver(files);
        let root = "import * as api from \"./api\"\n\
                    fn main() -> Int64 { let u = api.getUser(1) return u.id }";
        let a = analyze_linked(root, "main.vyrn", &LoadOptions::default(), &resolver);
        assert!(a.diagnostics.is_empty(), "diags: {:?}", a.diagnostics);

        // The namespace binding and its members are recorded.
        let nsi = a.namespaces.iter().find(|n| n.name == "api").expect("namespace `api` indexed");
        assert!(nsi.members.iter().any(|m| m.name == "getUser"), "getUser member");
        assert!(nsi.members.iter().any(|m| m.name == "User"), "User member");

        // Columns are 1-based; line 2 is the `fn main` body.
        let body = root.lines().nth(1).unwrap();
        let getuser_col = body.find("getUser").unwrap() + 1;

        // Completion at the `getUser` member position offers the module's exports.
        let comps = member_completions(&a, 2, getuser_col);
        assert!(comps.iter().any(|c| c.label == "getUser"), "completions: {comps:?}");
        assert!(comps.iter().all(|c| c.detail.contains("via namespace `api`")), "via-namespace note");

        // Go-to-definition on the `getUser` in `api.getUser` jumps into api.vyrn.
        let r = resolve(&a, 2, getuser_col).expect("resolve api.getUser");
        assert_eq!(r.name, "getUser");
        assert_eq!(r.target_file.as_deref(), Some("api.vyrn"), "cross-file go-to-def");
        assert!(r.hover.contains("via namespace `api`"), "hover note: {}", r.hover);

        // Hovering the `api` binding shows the namespace hover (not a value).
        let acol = body.find("api.").unwrap() + 1;
        let rn = resolve(&a, 2, acol).expect("resolve namespace name");
        assert!(rn.hover.contains("namespace `api`"), "namespace hover: {}", rn.hover);
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
        assert!(!labels.contains(&"pop"), "map must not offer `pop`: {labels:?}");
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
        assert_eq!(domain, &vec!["nav.about.label".to_string(), "nav.home.label".to_string()]);
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
        let src = format!(
            "{TRANSKEY}fn main() -> Int64 {{ let k: TransKey = \"\"  return 0 }}"
        );
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
}