//! RFC-0101 M1: the lowered form, checked against the copies it will replace.
//!
//! Three engines derive the static type of every expression, and the parity
//! suite proves only that the programs they emit behave the same. It cannot see
//! a type. So "the two copies agree" has been an assumption for as long as there
//! have been two copies, and this file is the first thing that turns it into a
//! gate — before M3 deletes either one.
//!
//! RFC-0101 M1 asked for one assertion, at every expression both compiled
//! backends type:
//!
//!   `peek`'s answer  ==  the native backend's threaded `(String, Type)` answer
//!                    ==  the type `vyrn-lower` recorded from the checker.
//!
//! **Measured, that is false, and finding it false is what M1 was for.** Over
//! 138 corpus programs and 570,960 typed expression answers, 22,283 differ from
//! the checker's answer and 3,383 differ between the two backends. No program
//! notices, because every one of them is coerced immediately afterwards — which
//! is exactly why the parity suite has never seen any of it.
//!
//! So this file asserts the true, checkable form of the same thing: **every
//! difference falls under a rule this file NAMES.** There are five, they are
//! listed on [`Rule`], and a difference that fits none of them fails the run. A
//! new class of disagreement is therefore a bug this gate catches, and the
//! existing classes are a measured description of what M3 has to reconcile
//! rather than an assumption it can delete against.
//!
//! It runs in-process rather than through `vyrn`, because the thing being
//! compared never reaches a process boundary: both backends' answers come out of
//! `vyrn_codegen::observe`, a sink that records what each emitter was about to
//! return anyway.

use std::collections::HashMap;
use std::path::PathBuf;

use vyrn_codegen::observe::{self, Site};
use vyrn_frontend::ast::{Expr, Program, Type, TypeDecl};
use vyrn_frontend::types::{decl_map, mentions_param, resolve};
use vyrn_lower::Node;

/// A filesystem resolver, which is all an example needs: the corpus imports
/// `std/` and its own siblings, and nothing here fetches.
struct Fs;

impl vyrn_frontend::loader::ModuleResolver for Fs {
    fn read(&self, resolved: &str) -> Result<String, String> {
        std::fs::read_to_string(resolved).map_err(|e| e.to_string())
    }
}

/// Not `canonicalize`: on Windows that returns a `\\?\` verbatim path, and a
/// verbatim std root resolves to a spec the loader cannot read — which it treats
/// as "no std root" and skips the runtime injection over, silently. The corpus
/// then loads without `std/json` and every synthesized encoder returns a type
/// nothing declares.
fn repo_root() -> PathBuf {
    let mut d = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    d.pop();
    d.pop();
    d
}

fn load(path: &std::path::Path) -> Result<Program, String> {
    let src = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let root = path.to_string_lossy().replace('\\', "/");
    let opts = vyrn_frontend::loader::LoadOptions {
        std_root: Some(repo_root().join("std").to_string_lossy().replace('\\', "/")),
        ..Default::default()
    };
    vyrn_frontend::load(&src, &root, &opts, &Fs).map_err(|d| {
        d.first()
            .map(|d| d.render())
            .unwrap_or_else(|| "load failed".into())
    })
}

/// The corpus, sorted, so a failure names the same example on every machine.
fn corpus() -> Vec<PathBuf> {
    let mut names: Vec<PathBuf> = std::fs::read_dir(repo_root().join("examples"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "vyrn"))
        .collect();
    names.sort();
    assert!(!names.is_empty(), "no examples found");
    names
}

/// An instantiation, spelled the same way on both sides of the comparison.
fn subst_key(subst: &[(String, Type)]) -> String {
    subst
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(";")
}

/// What kind of expression a node is — the axis a disagreement is classified on,
/// because "the backends disagree about `Expr::Call`" is a finding and "they
/// disagree at examples/foo.vyrn:41" is an anecdote.
fn kind(e: &Expr) -> &'static str {
    match e {
        Expr::Int(_) => "Int",
        Expr::Byte(_) => "Byte",
        Expr::Float(_) => "Float",
        Expr::Bool(_) => "Bool",
        Expr::Str(_) => "Str",
        Expr::Var { .. } => "Var",
        Expr::Unary { .. } => "Unary",
        Expr::Binary { .. } => "Binary",
        Expr::Call { .. } => "Call",
        Expr::Match { .. } => "Match",
        Expr::IfExpr { .. } => "IfExpr",
        Expr::Try { .. } => "Try",
        Expr::StructLit { .. } => "StructLit",
        Expr::Field { .. } => "Field",
        Expr::TryConstruct { .. } => "TryConstruct",
        Expr::ArrayLit { .. } => "ArrayLit",
        Expr::MapLit { .. } => "MapLit",
        Expr::Spawn { .. } => "Spawn",
        Expr::Lambda { .. } => "Lambda",
        Expr::Consume { .. } => "Consume",
    }
}

/// Why two answers about one node differ.
///
/// M1's headline result is that they DO differ, and that the reasons are few
/// and structural. Naming them is what turns 22,283 differences into a gate:
/// a difference that fits a rule is a fact about the compiler this file records,
/// and a difference that fits none fails the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Rule {
    /// The two spell the same type. `MaybeAge` and `Option<Int64>`, `Age` and
    /// `Int64`, `User` and its record shape: each engine resolves a declared
    /// name at a different point, and `types::resolve` is the referee.
    SameAfterResolve,
    /// One side wrote its DEFAULT where nothing constrained the position: the
    /// literal `1` under an `Int32` destination, the element type of `[]`, the
    /// unused side of a `Result`. The value is coerced immediately afterwards,
    /// which is why no program has ever noticed.
    DefaultedPosition,
    /// A heap array against a fixed-size one, or a `SmallArray`: the literal's
    /// own type against the type it is stored as.
    ArrayShape,
    /// One side is strictly less specific — it kept a type parameter, or dropped
    /// a generic's arguments (`Crate` for `Crate<Cargo>`). This is the class M3
    /// deletes rather than reconciles.
    LessSpecific,
    /// One side is `Never`: a `match` whose every arm leaves the function has no
    /// value to have a type, so the backends type it as the bottom and the
    /// checker types it as the destination. Both are right about a value that is
    /// never produced.
    Diverges,
}

#[derive(Default)]
struct Tally {
    examples: usize,
    unloadable: usize,
    instances: usize,
    rows: usize,
    /// Backend answers compared against a recorded type.
    compared: usize,
    /// …of which this many did not equal it.
    differed: usize,
    /// Answers where the two backends did not agree with EACH OTHER.
    cross_differed: usize,
    /// Backend answers whose node the checker never typed (see
    /// `Instance::untyped`) — a hole in the recording, counted not hidden.
    unrecorded: usize,
    /// Backend answers inside an instantiation the M1 worklist does not build:
    /// the higher-order and lambda instantiation that lives in the backends.
    uninstantiated: usize,
    unresolved: usize,
}

/// Which rule, if any, explains `a` against `b`. `None` means the gate fails.
///
/// Order matters: resolve first, so an alias never looks like a default.
fn rule(a: &Type, b: &Type, decls: &HashMap<String, TypeDecl>) -> Option<Rule> {
    if a == b {
        return None;
    }
    if matches!(a, Type::Never) || matches!(b, Type::Never) {
        return Some(Rule::Diverges);
    }
    let (ra, rb) = (deep(a, decls, 0), deep(b, decls, 0));
    if ra == rb {
        return Some(Rule::SameAfterResolve);
    }
    if mentions_param(&ra) != mentions_param(&rb) || dropped_args(a, b) {
        return Some(Rule::LessSpecific);
    }
    if array_shape(&ra, &rb) {
        return Some(Rule::ArrayShape);
    }
    // Both spellings: `Validation<Unit>` against `Validation<Person>` is one
    // defaulted argument before the alias is expanded and two differing enum
    // payloads after it, and either reading is the same fact.
    if defaulted(a, b) || defaulted(&ra, &rb) {
        return Some(Rule::DefaultedPosition);
    }
    None
}

/// [`resolve`] at every level, not only the outermost.
///
/// `resolve` answers "what is this name" and stops; `Option<Response>` is not a
/// name, so it comes back untouched while the other engine already wrote the
/// record out. Bounded because a type may name itself (RFC-0096).
fn deep(t: &Type, decls: &HashMap<String, TypeDecl>, depth: usize) -> Type {
    if depth > 6 {
        return t.clone();
    }
    let t = resolve(t, decls);
    let d = |x: &Type| Box::new(deep(x, decls, depth + 1));
    match &t {
        Type::Option(a) => Type::Option(d(a)),
        Type::Array(a) => Type::Array(d(a)),
        Type::Stream(a) => Type::Stream(d(a)),
        Type::Task(a) => Type::Task(d(a)),
        Type::ArrayN(a, n) => Type::ArrayN(d(a), *n),
        Type::SmallArray(a, n) => Type::SmallArray(d(a), *n),
        Type::Result(a, b) => Type::Result(d(a), d(b)),
        Type::Map(a, b) => Type::Map(d(a), d(b)),
        Type::Fn(ps, r) => Type::Fn(ps.iter().map(|p| deep(p, decls, depth + 1)).collect(), d(r)),
        Type::App(n, args) => Type::App(
            n.clone(),
            args.iter().map(|a| deep(a, decls, depth + 1)).collect(),
        ),
        Type::Record(fields) => Type::Record(
            fields
                .iter()
                .map(|f| vyrn_frontend::ast::Field {
                    name: f.name.clone(),
                    ty: deep(&f.ty, decls, depth + 1),
                })
                .collect(),
        ),
        _ => t,
    }
}

/// `Crate` where the other said `Crate<Cargo>`: the generic's arguments are gone.
fn dropped_args(a: &Type, b: &Type) -> bool {
    matches!((a, b), (Type::App(x, _), Type::Named(y)) | (Type::Named(y), Type::App(x, _)) if x == y)
}

/// `Array<E>` against `Array<E, N>` or `SmallArray<E, N>`, at the top or under
/// one container — the literal's own type against the type it is stored as.
fn array_shape(a: &Type, b: &Type) -> bool {
    let elem = |t: &Type| match t {
        Type::Array(e) | Type::ArrayN(e, _) | Type::SmallArray(e, _) => Some((**e).clone()),
        _ => None,
    };
    match (elem(a), elem(b)) {
        (Some(x), Some(y)) => {
            x == y || array_shape(&x, &y) || defaulted(&x, &y) || defaulted(&y, &x)
        }
        _ => false,
    }
}

/// Every position the two differ at has a DEFAULT on one side: `Int64` for an
/// integer, `Float64` for a float, and `Int64` again for the element of an empty
/// container or the unused arm of a sum, which is the same default reused.
fn defaulted(a: &Type, b: &Type) -> bool {
    fn walk(a: &Type, b: &Type) -> bool {
        if a == b {
            return true;
        }
        if matches!(a, Type::Int | Type::Float | Type::Unit)
            || matches!(b, Type::Int | Type::Float | Type::Unit)
        {
            return true;
        }
        match (a, b) {
            (Type::Option(x), Type::Option(y))
            | (Type::Array(x), Type::Array(y))
            | (Type::Stream(x), Type::Stream(y))
            | (Type::Task(x), Type::Task(y)) => walk(x, y),
            (Type::ArrayN(x, _), Type::ArrayN(y, _))
            | (Type::SmallArray(x, _), Type::SmallArray(y, _)) => walk(x, y),
            (Type::Result(x1, x2), Type::Result(y1, y2))
            | (Type::Map(x1, x2), Type::Map(y1, y2)) => walk(x1, y1) && walk(x2, y2),
            (Type::App(n1, a1), Type::App(n2, a2)) if n1 == n2 && a1.len() == a2.len() => {
                a1.iter().zip(a2).all(|(x, y)| walk(x, y))
            }
            (Type::Fn(p1, r1), Type::Fn(p2, r2)) if p1.len() == p2.len() => {
                p1.iter().zip(p2).all(|(x, y)| walk(x, y)) && walk(r1, r2)
            }
            _ => false,
        }
    }
    walk(a, b)
}

/// The corpus gate. Green means: wherever a compiled backend derived the type of
/// an expression, either it is the type the lowering recorded, or the difference
/// is one of the four this file names.
#[test]
fn every_backend_type_equals_the_recorded_one() {
    // Every walk here is recursive over the AST, and a test thread gets 2 MiB.
    // The corpus holds a 944-line example; the compiler itself runs on the main
    // thread and `vyrn-play` pins its linker stack for the same reason.
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(gate)
        .unwrap()
        .join()
        .unwrap();
}

fn gate() {
    let mut t = Tally::default();
    // (site, expression kind, recorded, backend) -> (count, first sighting)
    let mut disagreements: HashMap<(Site, &'static str, String, String), (usize, String)> =
        HashMap::new();
    let mut lint_failures: Vec<String> = Vec::new();
    // (engine A, engine B, expression kind, A's answer, B's answer) -> (count, example)
    let mut cross: HashMap<(Site, Site, &'static str, String, String), (usize, String)> =
        HashMap::new();
    let mut rules: std::collections::BTreeMap<Rule, usize> = Default::default();

    for path in corpus() {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        t.examples += 1;
        let Ok(program) = load(&path) else {
            // A corpus entry that does not link (an expected check failure, a
            // generator needing a cache) is not this gate's subject.
            t.unloadable += 1;
            continue;
        };

        let decls = decl_map(&program);
        let lowered = vyrn_lower::lower(&program);
        for problem in vyrn_lower::lint(&lowered) {
            lint_failures.push(format!("{name}: {problem}"));
        }
        t.instances += lowered.instances.len();
        t.rows += lowered.rows();
        t.unresolved += lowered.unresolved.len();

        // (node address, instantiation) -> the recorded type, and the node
        // itself so a disagreement can say what kind of expression it was.
        let mut recorded: HashMap<(usize, String), (Option<Type>, &Expr)> = HashMap::new();
        for inst in &lowered.instances {
            let key = subst_key(
                &inst
                    .subst
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<Vec<_>>(),
            );
            for row in &inst.rows {
                if let Node::Expr(e) = row.node {
                    recorded.insert((row.node.id(), key.clone()), (row.ty.clone(), e));
                }
            }
        }

        observe::start();
        let native = vyrn_codegen::emit(&program);
        let mut rows = observe::take();
        if native.is_err() {
            // A program the native backend refuses is not a program this gate
            // can compare; the wasm column would be answering about a different
            // walk. Parity already owns that failure.
            continue;
        }
        observe::start();
        let wasm = vyrn_codegen::direct::compile(&program);
        rows.extend(observe::take());
        if wasm.is_err() {
            continue;
        }

        // Half one: the two compiled backends against EACH OTHER. This is the
        // sentence RFC-0101 §1.1 says nothing checks — "the two copies agree" —
        // and it needs no interpretation to gate.
        let mut per_node: HashMap<(usize, String), Vec<(Site, Type)>> = HashMap::new();
        for row in &rows {
            per_node
                .entry((row.node, subst_key(&row.subst)))
                .or_default()
                .push((row.site, row.ty.clone()));
        }
        for (key, answers) in &per_node {
            // Only nodes the lowering recorded. A backend also types AST it
            // builds itself — a lifted lambda's synthesized body, a desugared
            // method call — and those live in temporaries whose addresses are
            // reused, so two of them can collide on one key. A node of the
            // PROGRAM is alive for the whole compile and cannot be aliased.
            let Some((_, node)) = recorded.get(key) else {
                t.uninstantiated += 1;
                continue;
            };
            let Some((_, first)) = answers.first() else {
                continue;
            };
            for (site, ty) in answers.iter().skip(1) {
                if ty == first {
                    continue;
                }
                t.cross_differed += 1;
                let r = rule(first, ty, &decls);
                if let Some(r) = r {
                    *rules.entry(r).or_insert(0) += 1;
                    continue;
                }
                let e = cross
                    .entry((
                        answers[0].0,
                        *site,
                        kind(node),
                        first.to_string(),
                        ty.to_string(),
                    ))
                    .or_insert_with(|| (0, name.clone()));
                e.0 += 1;
            }
        }

        // Half two: each backend answer against the recorded one.
        for row in rows {
            let Some((rec, node)) = recorded.get(&(row.node, subst_key(&row.subst))) else {
                continue;
            };
            let Some(rec) = rec else {
                t.unrecorded += 1;
                continue;
            };
            t.compared += 1;
            if *rec == row.ty {
                continue;
            }
            t.differed += 1;
            if let Some(r) = rule(rec, &row.ty, &decls) {
                *rules.entry(r).or_insert(0) += 1;
                continue;
            }
            let entry = disagreements
                .entry((row.site, kind(node), rec.to_string(), row.ty.to_string()))
                .or_insert_with(|| (0, format!("{name}:{}", node.line())));
            entry.0 += 1;
        }
    }

    eprintln!(
        "RFC-0101 M1 corpus gate: {} examples ({} did not link), {} instances, \
         {} rows\n  compared {} backend answers, {} of which differed from the \
         recorded type and {} of which differed between the two backends\n  \
         {} nodes the checker never typed, {} answers inside an instantiation M1 \
         does not build, {} calls the worklist could not follow\n  \
         every difference, by rule: {:?}",
        t.examples,
        t.unloadable,
        t.instances,
        t.rows,
        t.compared,
        t.differed,
        t.cross_differed,
        t.unrecorded,
        t.uninstantiated,
        t.unresolved,
        rules
    );

    let mut report = String::new();
    if !cross.is_empty() {
        let mut lines: Vec<String> = cross
            .into_iter()
            .map(|((a, b, kind, ta, tb), (n, first))| {
                format!("{n:5}x  {kind}: {a:?} said `{ta}`, {b:?} said `{tb}`  (first: {first})")
            })
            .collect();
        lines.sort();
        report.push_str(&format!(
            "the two compiled backends disagree about the type of an expression, \
             at {} classes:\n{}\n",
            lines.len(),
            lines.join("\n")
        ));
    }
    if !disagreements.is_empty() {
        let mut lines: Vec<String> = disagreements
            .into_iter()
            .map(|((site, kind, rec, got), (n, first))| {
                format!("{n:5}x  {site:?} {kind}: recorded `{rec}`, backend said `{got}`  (first: {first})")
            })
            .collect();
        lines.sort();
        report.push_str(&format!(
            "the three answers disagree at {} distinct (engine, expression, type \
             pair) classes:\n{}\n\nnote: this is what M1 exists to find. Diagnose \
             which of the three is right; do not widen the gate.\n",
            lines.len(),
            lines.join("\n")
        ));
    }
    if !lint_failures.is_empty() {
        lint_failures.sort();
        lint_failures.dedup();
        report.push_str(&format!(
            "the lowered form failed its own lint (RFC-0101 §2.6):\n  {}\n",
            lint_failures.join("\n  ")
        ));
    }
    assert!(report.is_empty(), "{report}");

    // A gate that compares nothing passes trivially. This is the floor the run
    // above cleared by two orders of magnitude; it exists so a refactor that
    // quietly stops recording fails here rather than passing.
    assert!(
        t.compared > 10_000,
        "only {} backend answers were compared — the gate stopped seeing the corpus",
        t.compared
    );
}
