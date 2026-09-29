//! The pipeline every host runs: the frontend's load and check, then this
//! crate's judgments over the checked program, in one list of diagnostics.

use std::collections::HashSet;

use vyrn_frontend::diagnostics::Diagnostic;
use vyrn_frontend::{ast, checker, floor, loader, movecheck, own, prof, symbols, types};

use crate::{core, typed};

/// Loads a multi-module program: parses `root_source`, resolves every
/// `import` transitively through `resolver`, links one [`ast::Program`], and
/// checks it.
pub fn load(
    root_source: &str,
    root_path: &str,
    opts: &loader::LoadOptions,
    resolver: &dyn loader::ModuleResolver,
) -> Result<ast::Program, Vec<Diagnostic>> {
    load_warned(root_source, root_path, opts, resolver).0
}

/// Like [`load`], and also returns the load's warnings. A warning never changes
/// an exit code or the program's output; a failed load returns none.
pub fn load_warned(
    root_source: &str,
    root_path: &str,
    opts: &loader::LoadOptions,
    resolver: &dyn loader::ModuleResolver,
) -> (Result<ast::Program, Vec<Diagnostic>>, loader::Warnings) {
    let load_span = prof::phase("load (total)");
    let (loaded, origins, warnings, _graph) =
        loader::load_with_origins(root_source, root_path, opts, resolver);
    drop(load_span);
    // The loader has already remapped its own diagnostics.
    let mut program = match loaded {
        Ok(p) => p,
        Err(diags) => return (Err(diags), warnings),
    };
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

/// Type-checks `program` and synthesizes what its builtins need into it
/// ([`vyrn_frontend::check_and_synthesize`]), then judges ownership and the
/// floor. Returns every diagnostic found.
pub fn check_and_synthesize(program: &mut ast::Program) -> Vec<Diagnostic> {
    let (mut diags, refused) = vyrn_frontend::check_and_synthesize(program);
    // One type record for the readers below. The synthesis is over, so no node
    // moves under its keys, and the guard closes before the caller can extend
    // the program again.
    let _held = checker::Held::open(program);
    // The checker's ownership refusals and the kernel's form one list, in
    // source order. The core builds bodies only for a program that type-checks.
    if diags.is_empty() {
        let _p = prof::phase("movecheck");
        diags.extend(refusals(program));
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
    // the check supplied the types.
    floor::settle(program, &mut diags);
    diags
}

/// Returns every ownership refusal a program earns, the must-use judgment's
/// and the kernel's, as one list in source order. `vyrn check` and the editor
/// both call it. The caller guarantees the program type-checks.
///
/// A kernel refusal is dropped at a line the must-use judgment already
/// refused, so one mistake is not said twice. It is also dropped when its
/// subject is a binding the must-use judgment names anywhere in the file: a
/// `Stream` closed twice is a must-use refusal and a use after a take at two
/// lines, and still one mistake.
pub fn refusals(program: &ast::Program) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let owed = typed::obligation::judge(program);
    let mustuse: HashSet<(Option<String>, String)> = owed
        .iter()
        .filter_map(|d| Some((d.file.clone(), subject(&d.message)?.to_string())))
        .collect();
    diags.extend(owed);
    // A generator's program skips the kernel: nothing prints its refusals, and
    // judging them costs the editor on every keystroke that re-runs one.
    if movecheck::in_comptime() {
        movecheck::in_source_order(&mut diags);
        return diags;
    }
    // The placer judges a core body for every instance, and the analysis is
    // handed on: a command's next `own::Memo` adopts it. Only this analysis
    // may reuse a judgment (`movecheck::reuse_judgments`). The kernel's list is
    // emptied first because an engine's or a generator's compile may have left
    // refusals there with no file.
    let _ = core::refusal_diagnostics();
    let _ = core::typed_diagnostics();
    let ownership = movecheck::judging(|| own::analyze(program));
    own::hand_on(program, &ownership);
    // A program the typed judgment refuses gets those refusals alone.
    let mut typed = core::typed_diagnostics();
    if !typed.is_empty() {
        let _ = core::refusal_diagnostics();
        movecheck::in_source_order(&mut typed);
        return typed;
    }
    let mut lines: HashSet<(Option<String>, usize)> = HashSet::new();
    for d in &diags {
        lines.insert((d.file.clone(), d.line));
    }
    diags.extend(core::refusal_diagnostics().into_iter().filter(|d| {
        !lines.contains(&(d.file.clone(), d.line))
            && !subject(&d.message)
                .is_some_and(|s| mustuse.contains(&(d.file.clone(), s.to_string())))
    }));
    movecheck::in_source_order(&mut diags);
    diags
}

/// Returns the binding a refusal is about: the root of the first path its
/// message quotes in backticks. Both passes write the subject first, so no
/// field has to be filled at every refusal site. A message that quotes nothing
/// has no subject and is never suppressed.
fn subject(message: &str) -> Option<&str> {
    let rest = message.split_once('`')?.1;
    let path = rest.split_once('`')?.0;
    let root = ast::root_of(path);
    (!root.is_empty() && root.chars().all(|c| c.is_alphanumeric() || c == '_')).then_some(root)
}

/// The ownership judgments the editor shows: [`refusals`] among the
/// diagnostics, and the placed analysis's memory rows on hover.
pub const JUDGE: symbols::Judge = symbols::Judge {
    refusals,
    ownership: own::analyze,
};

/// Builds the core of every body the checker typed in a refused program, and
/// returns the typed judgment's refusals of those bodies.
///
/// A refused function, impl method or module-state binding leaves the build,
/// and so does every function and binding that names or dispatches to one. A
/// program whose tests, benches or kept impl methods reach one, or with an impl
/// whose type has no key, is not built. The kernel's refusals are dropped,
/// because typing comes before the judgments.
fn lower_typed(program: &mut ast::Program, mut out: HashSet<String>) -> Vec<Diagnostic> {
    // A generator's own program is judged by the checker alone, as in
    // [`refusals`].
    if !own::placer_installed() || movecheck::in_comptime() {
        return Vec::new();
    }
    // A method of a refused impl is reached by name, or by a call the checker
    // dispatched on its receiver's type key. A receiver typed by a type
    // parameter may dispatch to any key.
    let record = checker::recorded(program);
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
    // The held record typed the functions just moved out.
    checker::hold_forget();
    let _ = core::refusal_diagnostics();
    let _ = core::typed_diagnostics();
    let _ = own::analyze(program);
    let _ = core::refusal_diagnostics();
    let typed = core::typed_diagnostics();
    let kept = std::mem::take(&mut program.functions);
    let mut back: Vec<(usize, ast::Function)> = at.into_iter().zip(kept).chain(gone).collect();
    back.sort_by_key(|(i, _)| *i);
    program.functions = back.into_iter().map(|(_, f)| f).collect();
    // The analysis held a record of the program without them.
    checker::hold_forget();
    typed
}
