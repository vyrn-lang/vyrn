//! Vyrn front end (v0 subset).
//!
//! Pipeline: source text -> [`lexer`] -> [`parser`] -> [`ast`] ->
//! [`checker`] -> [`interp`].
//!
//! The v0 subset is deliberately the "language in a day" core from the design
//! notes: `i64` integers, `bool`, `let`/`let mut`, arithmetic and comparisons,
//! `if`/`else`, `while`, functions, `return`, and a built-in `print`. The
//! advanced features in the RFCs (structural typing, validated types,
//! capabilities) are specified but NOT yet implemented here — this is the
//! skeleton they will hang from.

pub mod ast;
pub mod checker;
pub mod codec;
pub mod consteval;
pub mod diagnostics;
pub mod finite;
pub mod fmt;
pub mod hash;
pub mod interp;
pub mod lexer;
pub mod loader;
pub mod movecheck;
pub mod origin;
pub mod own;
pub mod parser;
pub mod regex;
pub mod schema;
pub mod schema_reflect;
pub mod symbols;
pub mod types;

// Re-export the symbol-query API at the crate root so the LSP can spell it as
// `vyrn_frontend::analyze` / `::resolve` / `::completions` and use the types
// directly. `diagnostics` (below) delegates to `symbols::analyze`, so the whole
// pipeline lives in one place.
pub use symbols::{
    analyze, analyze_linked, class_completions, class_token_hover, classify_at, completions,
    import_spec_at, member_completions, references, resolve, semantic_tokens,
    string_literal_completions, Analysis, Completion, LocalBinding, LocalKind, RefRange, Resolution,
    SemKind, SemMods, SemToken, Symbol, SymbolKind, TokenInfo,
};

// The canonical formatter (RFC-0017). `fmt` the module and `fmt` the function
// live in different namespaces, so `vyrn_frontend::fmt(src)` calls the function
// and `vyrn_frontend::fmt::` names the module.
pub use fmt::fmt;

// The names a `match` pattern binds (RFC-0023 uses this in codegen's lambda
// capture analysis, so it is re-exported at the crate root).
pub use movecheck::pattern_bindings;

/// Parse, type-check, and move-check `source`, returning the checked
/// [`ast::Program`].
///
/// On failure this returns the *first* problem rendered as `"line {N}: {message}"`
/// — the historical single-error surface. For all problems at once (with
/// structured positions), use [`diagnostics`].
pub fn check(source: &str) -> Result<ast::Program, String> {
    let diags = diagnostics(source);
    match diags.first() {
        None => {
            // No diagnostics, but we still need the program. Re-parse to obtain it;
            // since diagnostics() reported nothing, lex+parse+check+movecheck all
            // succeeded, so this is infallible in practice.
            let tokens = lexer::lex(source).expect("diagnostics reported no lex error");
            let program = parser::parse(tokens).expect("diagnostics reported no parse error");
            Ok(program)
        }
        Some(d) => Err(d.render()),
    }
}

/// Lex, parse, type-check, and move-check `source`, returning **all** problems
/// found as structured [`diagnostics::Diagnostic`]s.
///
/// Accumulation is bounded: a lex error is reported alone (the lexer stops at
/// the first illegal token); a parse error is recovered past (RFC-0006), so
/// several bad top-level declarations are each reported; once the source parses
/// cleanly, every type/ownership error across all functions and types is
/// reported — an error in one function does not suppress errors in the others.
pub fn diagnostics(source: &str) -> Vec<diagnostics::Diagnostic> {
    // The full pipeline (lex → parse → check → movecheck + symbol index) lives in
    // [`symbols::analyze`]; this is the diagnostics-only view of it, kept for the
    // CLI (`vyrn check`) and existing tests. Output is byte-identical to the
    // inlined version it replaced.
    symbols::analyze(source).diagnostics
}

/// Parse, check, then run `main` via the tree-walking interpreter.
///
/// Returns the integer value `main` returns (its exit code).
pub fn run(source: &str) -> Result<i64, String> {
    let program = check(source)?;
    interp::run(&program)
}

/// Load a multi-module program (RFC-0010): parse `root_source`, resolve every
/// `import` transitively through `resolver`, link into one [`ast::Program`],
/// then type-check and move-check it. Single-file programs (no imports) take
/// exactly the old path semantically — [`check`]/[`run`] remain the simple
/// single-source entry points.
pub fn load(
    root_source: &str,
    root_path: &str,
    opts: &loader::LoadOptions,
    resolver: &dyn loader::ModuleResolver,
) -> Result<ast::Program, Vec<diagnostics::Diagnostic>> {
    let (program, origins) = loader::load_with_origins(root_source, root_path, opts, resolver)?;
    let mut diags = checker::check_accum(&program);
    if diags.is_empty() {
        diags.extend(movecheck::check_accum(&program));
    }
    if diags.is_empty() {
        Ok(program)
    } else {
        // RFC-0033: a diagnostic in a synthesized generator module at an origin-
        // governed line is reported against its input file (`.vyx`, …) with the
        // generated location preserved as a note. Single-sourced in `origin`; the
        // LSP applies the same remap. A no-op when no generator emitted directives.
        if !origins.is_empty() {
            for d in &mut diags {
                origins.remap(d);
            }
        }
        Err(diags)
    }
}
