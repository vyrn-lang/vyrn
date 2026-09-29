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
pub mod jsondec;
pub mod lexer;
pub mod loader;
pub mod manifest;
pub mod movecheck;
pub mod origin;
pub mod own;
pub mod parser;
pub mod prelude;
pub mod prof;
pub mod project;
pub mod regex;
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

/// Loads a multi-module program: parses `root_source`, resolves every
/// `import` transitively through `resolver`, links one [`ast::Program`], and
/// checks it.
pub fn load(
    root_source: &str,
    root_path: &str,
    opts: &loader::LoadOptions,
    resolver: &dyn loader::ModuleResolver,
) -> Result<ast::Program, Vec<diagnostics::Diagnostic>> {
    load_warned(root_source, root_path, opts, resolver).0
}

/// Type-checks `program`, synthesizes what its builtins need into it, then
/// move-checks the result. Returns every diagnostic found.
///
/// An ordinary load and a generator re-loaded as its own root both
/// call it, so neither misses the synthesis. The synthesis sits here because
/// only here has the checker just typed every `fromJson` target while no
/// backend has built its function table from the program yet.
pub fn check_and_synthesize(program: &mut ast::Program) -> Vec<diagnostics::Diagnostic> {
    let check_span = prof::phase("check");
    let (mut diags, mut json_dec_types, derived, mut refused) =
        checker::check_accum_with_json_types(program);
    // What a `derive` generator writes joins the program and is checked
    // against it, so the second check's answers stand.
    if diags.is_empty() && !derived.is_empty() {
        match gen::derive(program, &derived) {
            Ok(fns) => {
                let at = program.functions.len();
                program.functions.extend(fns);
                // Parsed apart, so numbered from 1: renumbered, or their ids
                // would key the second check's types over the program's own.
                program.number_appended(at);
                // Only a whole check counts the program's own sites again.
                let (again, old_sites);
                (diags, again, refused, old_sites) = match checker::check_appended(program, at) {
                    Some((d, again, r)) => (d, again, r, 0),
                    None => {
                        let (d, j, again, r) = checker::check_accum_with_json_types(program);
                        json_dec_types = j;
                        (d, again, r, derived.len())
                    }
                };
                if diags.is_empty() && again.len() != old_sites {
                    diags.push(diagnostics::Diagnostic::error(
                        0,
                        0,
                        "check",
                        "a `derive` generator wrote a `derive` call".to_string(),
                    ));
                }
            }
            Err(e) => diags.push(diagnostics::Diagnostic::error(0, 0, "check", e)),
        }
    }
    drop(check_span);
    let synth_span = prof::phase("synthesize");
    let from = program.functions.len();
    if diags.is_empty() {
        let types = types::decl_map(program);
        match jsondec::decoders(&json_dec_types, &types) {
            Ok((fns, aliases)) => {
                program.functions.extend(fns);
                program.type_decls.extend(aliases);
            }
            Err(e) => diags.push(diagnostics::Diagnostic::error(0, 0, "check", e)),
        }
        // One constructor per `where` type, here for the JSON walks' reason.
        let have: std::collections::HashSet<&str> =
            program.functions.iter().map(|f| f.name.as_str()).collect();
        let fresh: Vec<_> = ctor::constructors(&types)
            .into_iter()
            .filter(|f| !have.contains(f.name.as_str()))
            .collect();
        program.functions.extend(fresh);
    }
    program.number_appended(from);
    // The checker's ownership refusals and the kernel's form one list, in
    // source order. The core builds bodies only for a program that type-checks.
    drop(synth_span);
    // One type record for the readers below. The synthesis is over, so no node
    // moves under its keys, and the guard closes before the caller can extend
    // the program again.
    let _held = checker::Held::open(program);
    if diags.is_empty() {
        let _p = prof::phase("movecheck");
        diags.extend(movecheck::refusals(program));
    } else if let Some(refused) = refused {
        let _p = prof::phase("lower typed");
        // Each typed refusal stands before the first of the checker's in its
        // file at a later line, so the list keeps the checker's own order.
        for d in lower_typed(program, refused) {
            let at = diags
                .iter()
                .position(|c| c.file == d.file && c.line > d.line)
                .or_else(|| diags.iter().rposition(|c| c.file == d.file).map(|i| i + 1))
                .unwrap_or(diags.len());
            diags.insert(at, d);
        }
    }
    // The floor row a judgment answers: the load deferred the decision until
    // the check supplied the types. Last, so a type error is not answered twice.
    if diags.is_empty() {
        let _p = prof::phase("floor");
        diags.extend(floor::decide(program));
    } else {
        // Drop the held decision so it cannot answer for the next program this
        // process checks without a load.
        floor::forget();
    }
    diags
}

/// Builds the core of every body the checker typed in a refused program, and
/// returns the typed judgment's refusals of those bodies.
///
/// A refused function, impl method or module-state binding leaves the build,
/// and so does every function and binding that names or dispatches to one. A
/// program whose tests, benches or kept impl methods reach one, or with an impl
/// whose type has no key, is not built. The kernel's refusals are dropped,
/// because typing comes before the judgments.
fn lower_typed(
    program: &mut ast::Program,
    mut out: std::collections::HashSet<String>,
) -> Vec<diagnostics::Diagnostic> {
    // A generator's own program is judged by the checker alone, as in
    // `movecheck::refusals`.
    if !own::placer_installed() || movecheck::in_comptime() {
        return Vec::new();
    }
    // A method of a refused impl is reached by name, or by a call the checker
    // dispatched on its receiver's type key. A receiver typed by a type
    // parameter may dispatch to any key.
    let record = checker::recorded(program);
    let impls = &program.impls;
    let refused_method =
        |out: &std::collections::HashSet<String>, name: &str, key: Option<&str>| {
            impls.iter().any(|i| {
                types::type_key(&i.ty).is_some_and(|k| {
                    key.is_none_or(|key| key == k)
                        && i.methods.iter().any(|m| {
                            m.name == name
                                && out.contains(&types::impl_method_name(&i.protocol, &k, name))
                        })
                })
            })
        };
    let dispatches = |b: &ast::Block, out: &std::collections::HashSet<String>| {
        let mut hit = false;
        for s in &b.stmts {
            ast::exprs_one(s, &mut |e, _| {
                let ast::Expr::Call { name, args, .. } = e else {
                    return;
                };
                let Some(recv) = args.first() else { return };
                hit |= match record.node_types.get(&recv.id()) {
                    Some(ast::Type::Param(_)) => refused_method(out, name, None),
                    Some(t) => {
                        types::type_key(t).is_some_and(|k| refused_method(out, name, Some(&k)))
                    }
                    None => false,
                };
            });
        }
        hit
    };
    // Each round moves at least one function or binding out, so the loop ends
    // within `program.functions.len() + program.globals.len()` rounds.
    loop {
        let fns = program
            .functions
            .iter()
            .filter(|f| {
                !out.contains(&f.name)
                    && (ast::names_any(&f.body, &f.params, &out) || dispatches(&f.body, &out))
            })
            .map(|f| f.name.clone());
        let globals = program
            .globals
            .iter()
            .filter(|g| !out.contains(&g.name) && ast::expr_names_any(&g.init, &out))
            .map(|g| g.name.clone());
        let more: Vec<String> = fns.chain(globals).collect();
        if more.is_empty() {
            break;
        }
        out.extend(more);
    }
    let blocks = program
        .tests
        .iter()
        .chain(&program.benches)
        .map(|t| &t.body);
    if blocks
        .into_iter()
        .any(|b| ast::names_any(b, &[], &out) || dispatches(b, &out))
        || impls.iter().any(|i| {
            types::type_key(&i.ty).is_none_or(|k| {
                let refused = i
                    .methods
                    .iter()
                    .any(|m| out.contains(&types::impl_method_name(&i.protocol, &k, &m.name)));
                !refused
                    && i.methods.iter().any(|m| {
                        ast::names_any(&m.body, &m.params, &out) || dispatches(&m.body, &out)
                    })
            })
        })
    {
        return Vec::new();
    }
    // The functions move out and back by value, in the source's order.
    let mut gone = Vec::new();
    let mut at = Vec::new();
    for (i, f) in std::mem::take(&mut program.functions)
        .into_iter()
        .enumerate()
    {
        if out.contains(&f.name) {
            gone.push((i, f));
        } else {
            at.push(i);
            program.functions.push(f);
        }
    }
    let _ = own::kernel_refusals();
    let _ = own::typed_refusals();
    let _ = own::analyze(program);
    let _ = own::kernel_refusals();
    let typed = own::typed_refusals();
    let kept = std::mem::take(&mut program.functions);
    let mut back: Vec<(usize, ast::Function)> = at.into_iter().zip(kept).chain(gone).collect();
    back.sort_by_key(|(i, _)| *i);
    program.functions = back.into_iter().map(|(_, f)| f).collect();
    typed
}

/// Like [`load`], and also returns the load's warnings. A warning never changes
/// an exit code or the program's output; a failed load returns none.
pub fn load_warned(
    root_source: &str,
    root_path: &str,
    opts: &loader::LoadOptions,
    resolver: &dyn loader::ModuleResolver,
) -> (
    Result<ast::Program, Vec<diagnostics::Diagnostic>>,
    loader::Warnings,
) {
    let load_span = prof::phase("load (total)");
    let (loaded, origins, warnings, _graph) =
        loader::load_with_origins(root_source, root_path, opts, resolver);
    drop(load_span);
    // The loader has already remapped its own diagnostics.
    let program = match loaded {
        Ok(p) => p,
        Err(diags) => return (Err(diags), warnings),
    };
    let mut program = program;
    let mut diags = check_and_synthesize(&mut program);
    if diags.is_empty() {
        (Ok(program), warnings)
    } else {
        // A diagnostic at an origin-governed line of a generated module moves to
        // its input file. `origin` states the rule; the LSP applies it too.
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
