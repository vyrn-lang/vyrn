//! Vyrn front end: the lexer, the parser, the checker, the module loader, and
//! the editor queries over a checked program.

pub mod artifacts;
pub mod ast;
pub mod audience;
pub mod checker;
pub mod codec;
pub mod consteval;
pub mod contracts;
pub mod core;
pub mod ctor;
pub mod declared;
pub mod diagnostics;
pub mod effects;
pub mod finite;
pub mod floor;
pub mod fmt;
pub mod gen;
pub mod hash;
pub mod lexer;
pub mod loader;
pub mod manifest;
pub mod movecheck;
pub mod origin;
pub mod own;
pub mod par;
pub mod parser;
pub mod prelude;
pub mod prim;
pub mod prof;
pub mod project;
pub mod regex;
pub mod rules;
pub mod schema;
pub mod schema_reflect;
pub mod session;
pub mod symbolmap;
pub mod symbols;
pub mod toolpin;
pub mod trap;
pub mod types;
pub mod validate;
pub mod vyx;

pub use symbols::{
    analyze, analyze_judged, analyze_linked, at_module_scope, class_completions, class_token_hover,
    classify_at, completions, import_spec_at, inlay_hints, member_completions, module_doc,
    references, references_to, resolve, semantic_tokens, string_literal_completions, Analysis,
    Completion, DocExport, InlayHint, Judge, LocalBinding, LocalKind, MemoryNote, ModuleDoc,
    RefRange, Resolution, SemKind, SemMods, SemToken, Symbol, SymbolKind, TokenInfo,
};
// Hover's type spelling, shared with the LSP's inlay hints.
pub use symbols::type_to_string;

// Contract knowledge lives here; the LSP and the CLI are adapters over it.
pub use contracts::{
    contract_completions, contract_fixes, contract_member_hover, discovered_roles, is_projection,
    load_contract, load_role_contract, role_for, roles_from_manifest, synthesized_members,
    ContractCompletion, ContractFix, ContractMemberView, ContractShape, ContractView, Role,
    RoleScope,
};

// `fmt` names both the module and the function; they live in different namespaces.
pub use fmt::fmt;

/// Parses, type-checks and move-checks `source`, and returns the checked program.
///
/// # Errors
///
/// Returns the first problem rendered as `"line {N}: {message}"`. Use
/// [`diagnostics`] for every problem with its position.
pub fn check(source: &str) -> Result<ast::Program, String> {
    let diags = diagnostics(source);
    match diags.first() {
        None => {
            // `diagnostics` reported nothing, so lexing and parsing succeed.
            let tokens = lexer::lex(source).expect("diagnostics reported no lex error");
            let mut program = parser::parse(tokens).expect("diagnostics reported no parse error");
            // A `where` type's predicate is a generated constructor, so this
            // path adds the constructors too. The JSON walks need the checker's
            // record, so they stay on the linked path.
            let types = types::decl_map(&program);
            let from = program.functions.len();
            program.functions.extend(ctor::constructors(&types));
            program.number_appended(from);
            Ok(program)
        }
        Some(d) => Err(d.render()),
    }
}

/// Lexes, parses and type-checks `source`, and returns every problem found.
///
/// A lex error is reported alone: the lexer stops at the first illegal token.
/// The parser recovers past a bad top-level declaration. Once the
/// source parses, every type error in every function is reported.
pub fn diagnostics(source: &str) -> Vec<diagnostics::Diagnostic> {
    symbols::analyze(source).diagnostics
}

/// Type-checks `program` and synthesizes what its builtins need into it, with
/// `engine` running each `derive` site's generator. `nest` names the generator
/// runs that enclose the check; a host's check is outermost, the default.
/// Returns the check's diagnostics, the refused set, the root's bindings
/// ([`checker::check_accum_with_sites`]) and the record of every body,
/// synthesized ones included, for the judgments to hold. The record is `None`
/// where an appended signature widens a capability, so a reader checks again.
/// The judgments that follow are `vyrn_lower::check_and_synthesize`'s.
///
/// An ordinary load and a generator re-loaded as its own root both
/// call it, so neither misses the synthesis. The synthesis sits here because
/// only here has the checker just typed every `derive` site while no
/// backend has built its function table from the program yet.
pub fn check_and_synthesize(
    program: &mut ast::Program,
    engine: Option<&gen::GenEngine>,
    nest: &loader::Nest,
) -> (
    Vec<diagnostics::Diagnostic>,
    Option<std::collections::HashSet<String>>,
    Vec<checker::LocalBinding>,
    Option<checker::Recorded>,
) {
    let check_span = prof::phase("check");
    let ((mut diags, derived, mut refused), binders, record) =
        checker::check_accum_with_sites(program);
    let mut record = Some(record);
    // What a `derive` generator writes joins the program and is checked
    // against it, so the second check's answers stand. The `where`
    // constructors join with it: `fromJson`'s decoders call the predicates.
    if diags.is_empty() && !derived.is_empty() {
        match gen::derive(program, &derived, engine, nest) {
            Ok((fns, decls)) => {
                let at = program.functions.len();
                program.functions.extend(fns);
                program.type_decls.extend(decls);
                program
                    .functions
                    .extend(ctor::constructors(&types::decl_map(program)));
                // Parsed apart, so numbered from 1: renumbered, or their ids
                // would key the second check's types over the program's own.
                program.number_appended(at);
                let again;
                let tail;
                let earlier = record.as_ref().map_or(&[][..], |r| &r.stored.dispatched);
                ((diags, again, refused), tail) = checker::check_appended(program, at, earlier);
                if let Some(rec) = record.as_mut() {
                    rec.extend(tail);
                }
                if diags.is_empty() && !again.is_empty() {
                    diags.push(rules::refuse!("check", 0, 0, DeriveWroteDerive));
                }
            }
            Err(d) => diags.push(d),
        }
    }
    drop(check_span);
    let synth_span = prof::phase("synthesize");
    let from = program.functions.len();
    if diags.is_empty() {
        let types = types::decl_map(program);
        // One constructor per `where` type, unless the `derive` join added it.
        let have: std::collections::HashSet<&str> =
            program.functions.iter().map(|f| f.name.as_str()).collect();
        let fresh: Vec<_> = ctor::constructors(&types)
            .into_iter()
            .filter(|f| !have.contains(f.name.as_str()))
            .collect();
        program.functions.extend(fresh);
    }
    program.number_appended(from);
    // The constructors are typed for the record alone: they are generated
    // from declarations that checked.
    if program.functions.len() > from {
        let (_, tail) = checker::check_appended(program, from, &[]);
        if let Some(rec) = record.as_mut() {
            rec.extend(tail);
        }
    }
    drop(synth_span);
    (diags, refused, binders, record)
}
