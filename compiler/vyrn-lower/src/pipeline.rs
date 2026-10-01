//! The pipeline every host runs: the frontend's load and check, then this
//! crate's judgments over the checked program, in one list of diagnostics.

use std::collections::HashSet;
use std::sync::Arc;

use vyrn_frontend::consteval::ConstVal;
use vyrn_frontend::diagnostics::Diagnostic;
use vyrn_frontend::gen::{GenEngine, GenError, GenInputs, GenOutput};
use vyrn_frontend::{ast, checker, floor, loader, movecheck, prof, symbols, types};

/// Loads a multi-module program: parses `root_source`, resolves every
/// `import` transitively through `resolver`, links one [`ast::Program`], and
/// checks it. `engine` runs every generator import and `derive` site; with
/// `None`, each one fails.
pub fn load(
    root_source: &str,
    root_path: &str,
    opts: &loader::LoadOptions,
    resolver: &dyn loader::ModuleResolver,
    engine: Option<&GenEngine>,
) -> Result<ast::Program, Vec<Diagnostic>> {
    let (loaded, _) = load_warned(root_source, root_path, opts, resolver, engine);
    loaded.map(|(program, _)| program)
}

/// Like [`load`], and also returns the World the check judged and the load's
/// warnings. The World answers for the program as returned; a host that
/// changes the program analyses it again ([`crate::analyze`]). A warning never
/// changes an exit code or the program's output; a failed load returns none.
pub fn load_warned(
    root_source: &str,
    root_path: &str,
    opts: &loader::LoadOptions,
    resolver: &dyn loader::ModuleResolver,
    engine: Option<&GenEngine>,
) -> (
    Result<(ast::Program, Arc<crate::World>), Vec<Diagnostic>>,
    loader::Warnings,
) {
    let load_span = prof::phase("load (total)");
    let (loaded, origins, warnings, _graph, pending) =
        loader::load_with_origins(root_source, root_path, opts, resolver, engine);
    drop(load_span);
    // The loader has already remapped its own diagnostics.
    let mut program = match loaded {
        Ok(p) => p,
        Err(diags) => return (Err(diags), warnings),
    };
    let (judged, world) = check(&mut program, engine, pending);
    let mut diags = judged.diagnostics;
    if let (true, Some(world)) = (diags.is_empty(), world) {
        (Ok((program, world)), warnings)
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

/// Type-checks `program` and synthesizes what its builtins need into it
/// ([`vyrn_frontend::check_and_synthesize`]) with `engine`, then judges
/// ownership and the floor. Returns every diagnostic found.
pub fn check_and_synthesize(
    program: &mut ast::Program,
    engine: Option<&GenEngine>,
) -> Vec<Diagnostic> {
    check(program, engine, None).0.diagnostics
}

/// [`check_and_synthesize`] with the floor decision the load returned, if any,
/// and the World the kernel judged, for a program that type-checks.
fn check(
    program: &mut ast::Program,
    engine: Option<&GenEngine>,
    pending: Option<floor::Pending>,
) -> (symbols::Judged, Option<Arc<crate::World>>) {
    let (mut diags, refused, binders, record) =
        vyrn_frontend::check_and_synthesize(program, engine, &Default::default());
    // One type record for the readers below: the check's own, or a new one
    // where the check made none. The synthesis is over, so no node moves under
    // its keys.
    let record = |p: &ast::Program| Arc::new(record.unwrap_or_else(|| checker::record(p)));
    // The checker's ownership refusals and the kernel's form one list, in
    // source order. The core builds bodies only for a program that type-checks.
    let mut memory = Default::default();
    let mut judged = None;
    if diags.is_empty() {
        let _p = prof::phase("movecheck");
        let (found, world) = refusals(program, record(program));
        diags.extend(found);
        memory = world.ownership.memory.clone();
        judged = Some(world);
    } else if let Some(refused) = refused {
        let _p = prof::phase("lower typed");
        // Each typed refusal stands before the first of the checker's in its
        // file at a later line, so the list keeps the checker's own order.
        let record = record(program);
        for d in lower_typed(program, refused, &record) {
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
    if let (true, Some(p), Some(world)) = (diags.is_empty(), pending, &judged) {
        let _p = prof::phase("floor");
        let reached = crate::effects::reaches(program, &world.ownership.record);
        diags.extend(floor::decide(p, Some(&reached)));
    }
    let linked = diags
        .iter()
        .find_map(|d| program.spellings.linked_in(&d.message));
    debug_assert!(linked.is_none(), "a refusal names the linked `{linked:?}`");
    let out = symbols::Judged {
        diagnostics: diags,
        binders,
        memory,
    };
    (out, judged)
}

/// Returns every ownership refusal a program earns, the kernel's, as one
/// list in source order, and the World the kernel judged. `record` is the
/// checker's record of `program`. The caller guarantees the program
/// type-checks.
pub fn refusals(
    program: &ast::Program,
    record: Arc<checker::Recorded>,
) -> (Vec<Diagnostic>, Arc<crate::World>) {
    // The placer judges a core body for every instance. Only this analysis
    // may reuse a judgment (`movecheck::reuse_judgments`).
    let world = crate::world::analyzed(program, record, true);
    // A program the typed judgment refuses gets those refusals alone.
    let mut diags = match world.typed_diagnostics() {
        [] => world.refusal_diagnostics(),
        typed => typed.to_vec(),
    };
    movecheck::in_source_order(&mut diags);
    (diags, world)
}

/// Wraps `run`, an engine that compiles and runs a generator, into the engine
/// a host passes to [`load`], which judges the generator's own program. The
/// judgments run inside `run`'s compile
/// (`direct::compile_gen_host`), which refuses the program the typed judgment
/// refused, or else the program with a must-use row. A program `run` declines
/// is refused with its must-use rows here, so it is refused whatever serves
/// it. The kernel's other refusals of a generator's program are not printed.
pub fn gen_engine(
    run: impl Fn(&ast::Program, &str, &[ConstVal], &GenInputs<'_>) -> Option<Result<GenOutput, GenError>>
        + Send
        + Sync
        + 'static,
) -> Box<GenEngine> {
    Box::new(move |program, name, args, inputs| {
        run(program, name, args, inputs).or_else(|| {
            let owed = crate::analyze(program).owed_diagnostics();
            (!owed.is_empty()).then_some(Err(GenError::Refused(owed)))
        })
    })
}

/// The pipeline the editor runs after its load: [`check_and_synthesize`] with
/// the load's floor decision.
pub const JUDGE: symbols::Judge = symbols::Judge {
    check: |program, engine, pending| check(program, engine, pending).0,
};

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
    mut out: HashSet<String>,
    record: &checker::Recorded,
) -> Vec<Diagnostic> {
    // A method of a refused impl is reached by name, or by a call the checker
    // dispatched on its receiver's type key. A receiver typed by a type
    // parameter may dispatch to any key.
    let impls = &program.impls;
    let refused_method = |out: &HashSet<String>, name: &str, key: Option<&str>| {
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
    let dispatches = |b: &ast::Block, out: &HashSet<String>| {
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
    // `record` typed the functions just moved out, so the analysis checks
    // again.
    let typed = crate::analyze(program).typed_diagnostics().to_vec();
    let kept = std::mem::take(&mut program.functions);
    let mut back: Vec<(usize, ast::Function)> = at.into_iter().zip(kept).chain(gone).collect();
    back.sort_by_key(|(i, _)| *i);
    program.functions = back.into_iter().map(|(_, f)| f).collect();
    typed
}
