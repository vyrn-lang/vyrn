//! Vyrn front end: the lexer, the parser, the checker, the module loader, and
//! the editor queries over a checked program.

pub mod artifacts;
pub mod ast;
pub mod audience;
pub mod checker;
pub mod codec;
pub mod consteval;
pub mod contracts;
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
pub mod parser;
pub mod prelude;
pub mod prim;
pub mod prof;
pub mod project;
pub mod regex;
pub mod rules;
pub mod schema;
pub mod schema_reflect;
pub mod symbolmap;
pub mod symbols;
pub mod toolpin;
pub mod trap;
pub mod types;
pub mod validate;
pub mod vyx;

pub use symbols::{
    analyze, analyze_linked, at_module_scope, class_completions, class_token_hover, classify_at,
    completions, import_spec_at, inlay_hints, member_completions, module_doc, references,
    references_to, resolve, semantic_tokens, string_literal_completions, Analysis, Completion,
    DocExport, InlayHint, LocalBinding, LocalKind, MemoryNote, ModuleDoc, RefRange, Resolution,
    SemKind, SemMods, SemToken, Symbol, SymbolKind, TokenInfo,
};
// Hover's type spelling, shared with the LSP's inlay hints.
pub use symbols::type_to_string;

// Contract knowledge lives here; the LSP and the CLI are adapters over it.
pub use contracts::{
    contract_completions, contract_fixes, contract_member_hover, contract_status, discovered_roles,
    is_projection, load_contract, load_role_contract, role_for, roles_from_manifest,
    synthesized_members, ContractCompletion, ContractFix, ContractMemberView, ContractShape,
    ContractView, MemberStatus, Role, RoleScope, StatusEntry,
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

/// Lexes, parses, type-checks and move-checks `source`, and returns every
/// problem found.
///
/// A lex error is reported alone: the lexer stops at the first illegal token.
/// The parser recovers past a bad top-level declaration. Once the
/// source parses, every type and ownership error in every function is reported.
pub fn diagnostics(source: &str) -> Vec<diagnostics::Diagnostic> {
    symbols::analyze(source).diagnostics
}

/// Type-checks `program` and synthesizes what its builtins need into it.
/// Returns the check's diagnostics and the refused set
/// ([`checker::check_accum_with_sites`]). The judgments that follow are
/// `vyrn_lower::check_and_synthesize`'s.
///
/// An ordinary load and a generator re-loaded as its own root both
/// call it, so neither misses the synthesis. The synthesis sits here because
/// only here has the checker just typed every `derive` site while no
/// backend has built its function table from the program yet.
pub fn check_and_synthesize(
    program: &mut ast::Program,
) -> (
    Vec<diagnostics::Diagnostic>,
    Option<std::collections::HashSet<String>>,
) {
    let check_span = prof::phase("check");
    let (mut diags, derived, mut refused) = checker::check_accum_with_sites(program);
    // What a `derive` generator writes joins the program and is checked
    // against it, so the second check's answers stand. The `where`
    // constructors join with it: `fromJson`'s decoders call the predicates.
    if diags.is_empty() && !derived.is_empty() {
        match gen::derive(program, &derived) {
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
                // Only a whole check counts the program's own sites again.
                let (again, old_sites);
                (diags, again, refused, old_sites) = match checker::check_appended(program, at) {
                    Some((d, again, r)) => (d, again, r, 0),
                    None => {
                        let (d, again, r) = checker::check_accum_with_sites(program);
                        (d, again, r, derived.len())
                    }
                };
                if diags.is_empty() && again.len() != old_sites {
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
    drop(synth_span);
    (diags, refused)
}

/// Checks a generator's own program: [`check_and_synthesize`], the must-use
/// judgment, and the floor. `movecheck::refusals` judges nothing else under
/// [`movecheck::comptime`].
pub(crate) fn check_generator(program: &mut ast::Program) -> Vec<diagnostics::Diagnostic> {
    movecheck::comptime(|| {
        let (mut diags, _) = check_and_synthesize(program);
        let _held = checker::Held::open(program);
        if diags.is_empty() {
            let _p = prof::phase("movecheck");
            diags.extend(movecheck::refusals(program));
        }
        floor::settle(program, &mut diags);
        diags
    })
}
