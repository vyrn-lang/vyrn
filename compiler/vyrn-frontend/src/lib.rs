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

pub mod artifacts;
pub mod ast;
pub mod audience;
pub mod checker;
pub mod codec;
pub mod consteval;
pub mod contracts;
pub mod declared;
pub mod diagnostics;
pub mod finite;
pub mod floor;
pub mod fmt;
pub mod hash;
pub mod interp;
pub mod jsondec;
pub mod jsonenc;
pub mod lexer;
pub mod loader;
pub mod manifest;
pub mod movecheck;
pub mod origin;
pub mod own;
pub mod parser;
/// The playground's host boundary — output, input and the clock — on the one
/// target that has no operating system to supply them (`wasm32-unknown-unknown`,
/// which `compiler/vyrn-play` builds). Absent everywhere else.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub mod playhost;
pub mod prelude;
pub mod project;
pub mod regex;
pub mod schema;
pub mod schema_reflect;
pub mod symbolmap;
pub mod symbols;
pub mod toolpin;
pub mod trap;
pub mod types;
pub mod vyx;

// Re-export the symbol-query API at the crate root so the LSP can spell it as
// `vyrn_frontend::analyze` / `::resolve` / `::completions` and use the types
// directly. `diagnostics` (below) delegates to `symbols::analyze`, so the whole
// pipeline lives in one place.
pub use symbols::{
    analyze, analyze_linked, at_module_scope, class_completions, class_token_hover, classify_at,
    completions, import_spec_at, inlay_hints, member_completions, module_doc, references,
    references_to, resolve, semantic_tokens, string_literal_completions, Analysis, Completion,
    DocExport, InlayHint, LocalBinding, LocalKind, MemoryNote, ModuleDoc, RefRange, Resolution,
    SemKind, SemMods, SemToken, Symbol, SymbolKind, TokenInfo,
};
// The type spelling hover writes — one renderer, shared with the LSP's inlay
// type hints (an anonymous enum reads as its variant arms in both).
pub use symbols::type_to_string;

// RFC-0071 M4: role → contract resolution and the editor queries over a
// resolved contract. Spelled `vyrn_frontend::contracts::` by the LSP and the
// CLI, so the contract knowledge stays in ONE place and both are adapters.
pub use contracts::{
    contract_completions, contract_fixes, contract_member_hover, contract_status, discovered_roles,
    is_projection, load_contract, load_role_contract, role_for, roles_from_manifest,
    synthesized_members, ContractCompletion, ContractFix, ContractMemberView, ContractShape,
    ContractView, MemberStatus, Role, RoleScope, StatusEntry,
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
    load_warned(root_source, root_path, opts, resolver).0
}

/// Like [`load`], but also returns the load's WARNINGS (RFC-0071 M2b) —
/// diagnostics about a program that compiled.
///
/// This is the shape every tool wants: a warning is advice, so it accompanies
/// the program rather than replacing it, and printing it must never change an
/// exit code or a byte of the program's own output. `load` stays as the
/// warning-oblivious entry point for callers that only care whether it built.
/// Type-check `program`, synthesize what its builtins need into it, then
/// move-check the result. Returns every diagnostic found.
///
/// The one place a LINKED program becomes a runnable one, and it has to be a
/// single function because there are two callers: an ordinary load, and a
/// generator re-loaded as its own root (RFC-0021), which RFC-0076 then compiles to
/// wasm. A generator that missed the synthesis compiled to a module calling a
/// function that was never emitted — found by `genwasm`, exactly the tier meant to
/// find it.
///
/// The synthesis sits HERE and nowhere else because this is the only point with
/// both halves of what it needs: the checker has just supplied the static type of
/// every `toJson` argument (RFC-0078 M2b), and no engine has yet built its function
/// table — all three build one, once, from a `&Program`. Afterwards the encoders
/// are ordinary Vyrn: move-checked below with everything else, and lowered by every
/// backend as source it cannot tell apart from the user's.
pub fn check_and_synthesize(program: &mut ast::Program) -> Vec<diagnostics::Diagnostic> {
    let (mut diags, json_types, json_dec_types) = checker::check_accum_with_json_types(program);
    if diags.is_empty() {
        let types = types::decl_map(program);
        match jsonenc::encoders(&json_types, &types) {
            Ok(fns) => program.functions.extend(fns),
            Err(e) => diags.push(diagnostics::Diagnostic::error(0, 0, "check", e)),
        }
        match jsondec::decoders(&json_dec_types, &types) {
            Ok((fns, aliases)) => {
                program.functions.extend(fns);
                program.type_decls.extend(aliases);
            }
            Err(e) => diags.push(diagnostics::Diagnostic::error(0, 0, "check", e)),
        }
    }
    if diags.is_empty() {
        diags.extend(movecheck::check_accum(program));
    }
    diags
}

pub fn load_warned(
    root_source: &str,
    root_path: &str,
    opts: &loader::LoadOptions,
    resolver: &dyn loader::ModuleResolver,
) -> (
    Result<ast::Program, Vec<diagnostics::Diagnostic>>,
    loader::Warnings,
) {
    let (loaded, origins, warnings, _graph) =
        loader::load_with_origins(root_source, root_path, opts, resolver);
    // RFC-0053: load/lex/parse diagnostics are already remapped by the loader.
    let program = match loaded {
        Ok(p) => p,
        Err(diags) => return (Err(diags), warnings),
    };
    let mut program = program;
    let mut diags = check_and_synthesize(&mut program);
    if diags.is_empty() {
        (Ok(program), warnings)
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
        // A program that failed to compile gets errors, not advice: dropping the
        // warnings here keeps the failure output about the failure.
        (Err(diags), Vec::new())
    }
}
