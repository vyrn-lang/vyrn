//! Gates the lowered form against the direct backend.
//!
//! At every expression the backend types, its answer (recorded by `vyrn_codegen::observe`)
//! must equal one member of the pair `vyrn-lower` recorded from the checker - the type the
//! value has, or the type it must end up as - or fall under a rule on [`Rule`]. The backend's
//! instance list must equal the lowering's, up to the rules on [`InstRule`]. At every
//! `Call` and `Prim` of the core the emitter emits, the type it derives from the callee
//! or the operator must equal the checker's producer type on the row, up to a [`Rule`];
//! that count grows as the core takes bodies. It runs in-process because the compared
//! answers never cross a process boundary.

use vyrn_frontend::loader::DiskResolver;
use vyrn_frontend::project::Memo;

use std::collections::HashMap;
use std::path::PathBuf;

use vyrn_codegen::observe::{self, Site};
use vyrn_frontend::ast::{Expr, NodeId, Program, Type, TypeDecl};
use vyrn_frontend::types::{decl_map, mentions_param, resolve};
use vyrn_lower::NodeTypes;

/// Not `canonicalize`: on Windows that returns a `\\?\` verbatim path, which the
/// loader silently treats as "no std root", so the corpus loads without `std/json`.
fn repo_root() -> PathBuf {
    let mut d = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    d.pop();
    d.pop();
    d
}

fn load(path: &std::path::Path) -> Result<(Program, Memo), String> {
    let src = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let root = path.to_string_lossy().replace('\\', "/");
    let opts = vyrn_frontend::loader::LoadOptions {
        std_root: Some(repo_root().join("std").to_string_lossy().replace('\\', "/")),
        ..Default::default()
    };
    Memo::load(|| vyrn_frontend::load(&src, &root, &opts, &DiskResolver)).map_err(|d| {
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

fn subst_key(subst: &[(String, Type)]) -> String {
    subst
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(";")
}

/// Spells an instance as its callee and type arguments resolved through every alias
/// ([`deep`]), because the backend and the lowering reach one instance by different
/// routes (`Age` and `Int64`). Not `mangle_name`: two records mangle alike (#165).
fn inst_key(name: &str, args: &[Type], decls: &HashMap<String, TypeDecl>) -> String {
    if args.is_empty() {
        return name.to_string();
    }
    let args: Vec<String> = args.iter().map(|a| deep(a, decls, 0).to_string()).collect();
    format!("{name}<{}>", args.join(", "))
}

/// The axis a disagreement is classified on.
fn kind(e: &Expr) -> &'static str {
    match e {
        Expr::Int(_, _) => "Int",
        Expr::Byte(_, _) => "Byte",
        Expr::Float(_, _) => "Float",
        Expr::Bool(_, _) => "Bool",
        Expr::Str(_, _) => "Str",
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
        Expr::Lambda { .. } => "Lambda",
        Expr::Consume { .. } => "Consume",
    }
}

/// Why two answers about one node differ. A difference that fits no rule fails the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Rule {
    /// The two spell the same type. `MaybeAge` and `Option<Int64>`, `Age` and
    /// `Int64`, `User` and its record shape: each engine resolves a declared
    /// name at a different point, and `types::resolve` is the referee.
    SameAfterResolve,
    /// One side wrote its DEFAULT where nothing constrained the position: the
    /// element type of `[]`, the unused side of a `Result`. The value is coerced
    /// immediately afterwards, so no program notices. The positions are ones the
    /// form's has-derivation does not settle: a `match` arm's, a wasm local's.
    DefaultedPosition,
    /// A heap array against a fixed-size one, or a `SmallArray`: the literal's
    /// own type against the type it is stored as.
    ArrayShape,
    /// One side is strictly less specific: it kept a type parameter, or dropped
    /// a generic's arguments (`Crate` for `Crate<Cargo>`).
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
    /// ...of which this many answered the other member of the pair: the type the
    /// value has, where the recorded one is the type it must end up as ([A16]).
    answered_has: usize,
    /// ...and this many equalled neither member.
    differed: usize,
    /// Core right-hand sides the emitter typed from the callee or the operator,
    /// compared against the checker's producer type on the row.
    typed: usize,
    /// ...of which this many differed under a [`Rule`].
    typed_ruled: usize,
    /// Answers where the two backends did not agree with EACH OTHER.
    cross_differed: usize,
    /// Backend answers whose node the form holds but the checker never typed.
    /// The run prints the class: a `Var` the checker resolves by name, such as
    /// the receiver of `xs.pop()` and the temporaries `parser::place_receiver` hoists.
    unrecorded: usize,
    /// Backend answers about a node the lowering recorded, under an
    /// instantiation it did not build. Asserted zero.
    uninstantiated: usize,
    /// Backend answers about a node the lowering never recorded: AST the backend
    /// built itself. The count is not reproducible; see the ceiling in [`gate`].
    synthesized: usize,
    /// ...of which this many were given inside a copy of a lambda's body.
    in_lambda: usize,
    /// ...and this many inside a copy of a `where` predicate.
    in_predicate: usize,
    /// ...and this many were `peek`'s. Only AST the backend built itself goes to
    /// `peek`, and the emitter builds none.
    peek_off: usize,
    /// Instantiations the backend emitted that the lowering's worklist lacks.
    missing: usize,
    /// ...and the other direction, which each needs an [`InstRule`].
    extra: usize,
    unresolved: usize,
    /// Boundary crossings that took the rung the plan places,
    /// and the ones that took another.
    rungs_planned: usize,
    rungs_unruled: usize,
    /// ...and of those, a pair the plan refuses that an engine walked past, or the
    /// reverse: a compile error on one target and a bit reinterpretation on another.
    rungs_terminal: usize,
}

/// Why the lowering's instance list and a backend's differ. A difference that fits
/// no rule fails the run; each rule is a target fact, not a language decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum InstRule {
    /// A `gen fn` runs in the compiler's interpreter at generation
    /// time and is never called in a shipped binary, so the emitter emits no
    /// body for it. The lowering has one, because the program does.
    GenFn,
}

/// Which rule, if any, explains `a` against `b`. `None` means the gate fails.
/// Resolve first, so an alias never looks like a default.
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

/// [`resolve`] at every level, not only the outermost: `Option<Response>` is not
/// a name, so `resolve` leaves the record inside it. Bounded because a type may
/// name itself.
fn deep(t: &Type, decls: &HashMap<String, TypeDecl>, depth: usize) -> Type {
    if depth > 6 {
        return t.clone();
    }
    let t = resolve(t, decls);
    let d = |x: &Type| Box::new(deep(x, decls, depth + 1));
    match &t {
        Type::Array(a) => Type::Array(d(a)),
        Type::Stream(a) => Type::Stream(d(a)),
        Type::ArrayN(a, n) => Type::ArrayN(d(a), *n),
        Type::SmallArray(a, n) => Type::SmallArray(d(a), *n),
        Type::Map(a, b) => Type::Map(d(a), d(b)),
        // `resolve` answers `Enum` for `Option` and `Result`, so a payload that is
        // a name stays unresolved unless the walk descends here.
        Type::Enum(vs) => Type::Enum(
            vs.iter()
                .map(|v| vyrn_frontend::ast::EnumVariant {
                    name: v.name.clone(),
                    payload: v
                        .payload
                        .iter()
                        .map(|p| deep(p, decls, depth + 1))
                        .collect(),
                })
                .collect(),
        ),
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
/// one container.
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
            (Type::Array(x), Type::Array(y)) | (Type::Stream(x), Type::Stream(y)) => walk(x, y),
            (Type::ArrayN(x, _), Type::ArrayN(y, _))
            | (Type::SmallArray(x, _), Type::SmallArray(y, _)) => walk(x, y),
            (Type::Map(x1, x2), Type::Map(y1, y2)) => walk(x1, y1) && walk(x2, y2),
            // `Option` and `Result` resolve to variant lists, so the defaulted half
            // is a payload rather than a type argument.
            (Type::Enum(v1), Type::Enum(v2)) if v1.len() == v2.len() => {
                v1.iter().zip(v2).all(|(x, y)| {
                    x.name == y.name
                        && x.payload.len() == y.payload.len()
                        && x.payload.iter().zip(&y.payload).all(|(p, q)| walk(p, q))
                })
            }
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

/// Each expression of `facts` by node, with the pair it carries: the type
/// it must end up as, and the type it has where that differs.
#[allow(clippy::type_complexity)]
fn pairs<'a>(facts: &NodeTypes<'a>) -> Vec<(NodeId, (Option<Type>, Option<Type>), &'a Expr)> {
    facts
        .exprs
        .iter()
        .map(|(e, _)| {
            let id = e.id();
            let ty = facts.types.get(&id);
            let has = facts.produced.get(&id).filter(|h| Some(*h) != ty);
            (id, (ty.cloned(), has.cloned()), *e)
        })
        .collect()
}

#[test]
fn every_backend_type_equals_the_recorded_one() {
    // Every walk here recurses over the AST, and a test thread gets 2 MiB.
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(gate)
        .unwrap()
        .join()
        .unwrap();
}

fn gate() {
    // Without this every generator example fails to link and the gate silently
    // measures a smaller corpus.
    vyrn_genwasm::install();
    // Without this the placer never runs, `core::BODIES` stays empty and the
    // backend answers from the AST alone: the gate measures one compiler against
    // itself.
    vyrn_lower::install();
    let mut t = Tally::default();
    // The residue by engine and expression kind. Reported, never asserted: the
    // raw count is not reproducible (see [`Tally::synthesized`]).
    let mut residue: std::collections::BTreeMap<String, usize> = Default::default();
    let mut residue_ex: std::collections::BTreeMap<String, String> = Default::default();
    // ...and the rows the form holds and has no type for, on the same axis.
    let mut untyped: std::collections::BTreeMap<String, usize> = Default::default();
    // (site, expression kind, recorded, backend) -> (count, first sighting)
    let mut disagreements: HashMap<(Site, &'static str, String, String), (usize, String)> =
        HashMap::new();
    let mut lint_failures: Vec<String> = Vec::new();
    // (engine A, engine B, expression kind, A's answer, B's answer) -> (count, example)
    let mut cross: HashMap<(Site, Site, &'static str, String, String), (usize, String)> =
        HashMap::new();
    let mut rules: std::collections::BTreeMap<Rule, usize> = Default::default();
    // Release steps placed inside a lambda body: a fact about the placement no
    // engine reports. At zero, a lifted lambda's shell has nothing to unwind.
    let mut rel_lambda = 0usize;
    let mut inst_rules: std::collections::BTreeMap<InstRule, usize> = Default::default();
    // (engine, planned rung, rung taken) -> count, and the ones no rule explains.
    let mut ladder: std::collections::BTreeMap<
        (Site, vyrn_codegen::Rung, vyrn_codegen::Rung),
        usize,
    > = Default::default();
    #[allow(clippy::type_complexity)]
    let mut unruled: std::collections::BTreeMap<
        (Site, vyrn_codegen::Rung, vyrn_codegen::Rung),
        std::collections::BTreeSet<String>,
    > = Default::default();
    // (site, spelled instance) -> example.
    let mut missing: std::collections::BTreeMap<(Site, String), String> = Default::default();
    // An instance the lowering has that no rule explains away -> example.
    let mut extra: std::collections::BTreeMap<String, String> = Default::default();
    // (kind, callee or operator, checker's type, emitter's type) -> (count, example).
    #[allow(clippy::type_complexity)]
    let mut typing_diffs: std::collections::BTreeMap<
        (&'static str, String, String, String),
        (usize, String),
    > = Default::default();

    for path in corpus() {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        t.examples += 1;
        // `memo` holds one expansion per access site, shared by the lowering and
        // the backend; without it each walk lands on its own addresses.
        let Ok((program, memo)) = load(&path) else {
            // An expected check failure or a generator needing a cache.
            t.unloadable += 1;
            continue;
        };

        let decls = decl_map(&program);
        let lowered = vyrn_lower::lower(&program);
        for problem in vyrn_lower::lint(&lowered) {
            lint_failures.push(format!("{name}: {problem}"));
        }
        t.instances += lowered.instances.len();
        t.rows += lowered.exprs();
        // The only legal reason the worklist stops following a call is the
        // monomorphization bound (`examples/polyrecursion.vyrn` needs it).
        for u in &lowered.unresolved {
            t.unresolved += 1;
            assert_eq!(
                u.why,
                vyrn_lower::Why::PastTheLimit,
                "{name}: the worklist stopped at `{}` -> `{}`: {}",
                u.caller,
                u.callee,
                u.why
            );
        }

        // (node address, instantiation) -> (end-up type, has-type, node).
        let mut recorded: HashMap<(NodeId, String), (Option<Type>, Option<Type>, &Expr)> =
            HashMap::new();
        // Every node the lowering recorded, under any instantiation: it
        // separates an unbuilt substitution from AST that is not in the program.
        let mut walked: std::collections::HashSet<NodeId> = Default::default();
        for inst in &lowered.instances {
            let key = subst_key(
                &inst
                    .subst
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<Vec<_>>(),
            );
            for (id, pair, e) in pairs(&inst.facts) {
                recorded.insert((id, key.clone()), (pair.0, pair.1, e));
                walked.insert(id);
            }
        }
        // Module-state initializers are lowered under no substitution.
        for (id, pair, e) in pairs(&lowered.globals) {
            recorded.insert((id, String::new()), (pair.0, pair.1, e));
            walked.insert(id);
        }
        // A `where` predicate lives on a declaration, which has no type
        // parameters, but it is walked inside whatever body the boundary is in:
        // an answer about one is looked up with the instantiation dropped.
        let mut predicate_nodes: std::collections::HashSet<NodeId> = Default::default();
        for (id, pair, e) in pairs(&lowered.predicates) {
            recorded.insert((id, String::new()), (pair.0, pair.1, e));
            walked.insert(id);
            predicate_nodes.insert(id);
        }
        let at = |node: NodeId, subst: &str| {
            if predicate_nodes.contains(&node) {
                (node, String::new())
            } else {
                (node, subst.to_string())
            }
        };

        let mut lowering: std::collections::BTreeSet<String> = Default::default();
        for inst in &lowered.instances {
            lowering.insert(inst_key(&inst.func.name, &inst.type_args, &decls));
        }

        for inst in &lowered.instances {
            for rel in &inst.releases {
                if lowered.lambda_bodies.contains(&rel.site) {
                    rel_lambda += 1;
                }
            }
        }

        observe::start();
        let wasm = vyrn_codegen::direct::compile(&program, &memo);
        let rows = observe::take();
        let insts = observe::take_insts();
        let crossings = observe::take_crossings();
        let typings = observe::take_typings();
        if wasm.is_err() {
            continue;
        }

        // Every boundary crossing against the plan, keyed by the type pair
        // because `coerce` is reached from call sites no node identifies.
        for c in &crossings {
            let planned = vyrn_codegen::coerce_plan(&c.from, &c.to, &decls);
            *ladder.entry((c.site, planned, c.rung)).or_insert(0) += 1;
            if planned == c.rung {
                t.rungs_planned += 1;
                continue;
            }
            t.rungs_unruled += 1;
            if planned == vyrn_codegen::Rung::Refuse || c.rung == vyrn_codegen::Rung::Refuse {
                t.rungs_terminal += 1;
            }
            unruled
                .entry((c.site, planned, c.rung))
                .or_default()
                .insert(format!("`{}` -> `{}` ({name})", c.from, c.to));
        }

        // The emitter's own typing of each core right-hand side against the
        // checker's producer type on the row.
        for ty in &typings {
            t.typed += 1;
            if ty.got == ty.checker {
                continue;
            }
            if let Some(r) = rule(&ty.checker, &ty.got, &decls) {
                t.typed_ruled += 1;
                *rules.entry(r).or_insert(0) += 1;
                continue;
            }
            typing_diffs
                .entry((
                    ty.kind,
                    ty.what.clone(),
                    ty.checker.to_string(),
                    ty.got.to_string(),
                ))
                .or_insert_with(|| (0, name.clone()))
                .0 += 1;
        }

        // The lowering's worklist against the backend's. A body only the backend
        // has is a hole in the lowering; a body only the lowering has is a
        // target fact and must name its rule.
        let mut backend: std::collections::BTreeSet<(Site, String)> = Default::default();
        for i in &insts {
            let k = inst_key(&i.name, &i.args, &decls);
            if !lowering.contains(&k) {
                t.missing += 1;
                missing.entry((i.site, k.clone())).or_insert(name.clone());
            }
            backend.insert((i.site, k));
        }
        let by_name: HashMap<&str, &vyrn_frontend::ast::Function> = program
            .functions
            .iter()
            .map(|f| (f.name.as_str(), f))
            .collect();
        for k in &lowering {
            if backend.iter().any(|(_, b)| b == k) {
                continue;
            }
            t.extra += 1;
            let f = k.split('<').next().unwrap_or(k);
            // No rule for the higher-order shell on purpose: a specialization
            // keys back to the same (callee, type arguments) the lowering built,
            // so a shell that shows up here is a real difference.
            match by_name.get(f) {
                Some(f) if f.is_gen => *inst_rules.entry(InstRule::GenFn).or_insert(0) += 1,
                _ => {
                    extra.entry(k.clone()).or_insert(name.clone());
                }
            }
        }

        // The engines' answers about one node against each other.
        let mut per_node: HashMap<(NodeId, String), Vec<(Site, Type, &'static str, &'static str)>> =
            HashMap::new();
        for row in &rows {
            per_node
                .entry(at(row.node, &subst_key(&row.subst)))
                .or_default()
                .push((row.site, row.ty.clone(), row.kind, row.ctx));
        }
        for (key, answers) in &per_node {
            // Only nodes the lowering recorded. AST a backend builds itself lives
            // in temporaries whose addresses are reused, so two can collide on
            // one key; a program node lives for the whole compile.
            let Some((_, _, node)) = recorded.get(key) else {
                if walked.contains(&key.0) {
                    t.uninstantiated += 1;
                } else {
                    t.synthesized += 1;
                    for (site, _, kind, ctx) in answers {
                        match *ctx {
                            "lambda" => t.in_lambda += 1,
                            "pred" => t.in_predicate += 1,
                            _ => {}
                        }
                        if *site == Site::Peek {
                            t.peek_off += 1;
                        }
                        // After `~`: the engine copy the answer was given inside.
                        let k = format!(
                            "{site:?}/{kind}{}{ctx}",
                            if ctx.is_empty() { "" } else { "~" }
                        );
                        *residue.entry(k.clone()).or_insert(0) += 1;
                        residue_ex.entry(k).or_insert_with(|| name.clone());
                    }
                }
                continue;
            };
            let Some((_, first, ..)) = answers.first() else {
                continue;
            };
            for (site, ty, ..) in answers.iter().skip(1) {
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

        // Each backend answer against the recorded pair.
        for row in rows {
            let Some((rec, has, node)) = recorded.get(&at(row.node, &subst_key(&row.subst))) else {
                continue;
            };
            let Some(rec) = rec else {
                t.unrecorded += 1;
                *untyped
                    .entry(format!("{:?}/{}", row.site, kind(node)))
                    .or_insert(0) += 1;
                continue;
            };
            t.compared += 1;
            if *rec == row.ty {
                continue;
            }
            // A backend answering the has-type answers the other member of the
            // pair, which is not a disagreement ([A16]).
            if has.as_ref() == Some(&row.ty) {
                t.answered_has += 1;
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
        "corpus gate: {} examples ({} did not link), {} instances, \
         {} rows\n  compared {} backend answers: {} answered the pair's has-type, \
         {} equalled neither member, and {} differed between the two backends\n  \
         {} nodes the checker never typed, {} answers under an instantiation the \
         lowering does not build, {} about AST no instantiation of the program \
         holds, {} calls the worklist stopped following\n  \
         every difference, by rule: {:?}\n  instantiations: {} the backends \
         emitted and the lowering does not have, {} the other way, by rule: {:?}",
        t.examples,
        t.unloadable,
        t.instances,
        t.rows,
        t.compared,
        t.answered_has,
        t.differed,
        t.cross_differed,
        t.unrecorded,
        t.uninstantiated,
        t.synthesized,
        t.unresolved,
        rules,
        t.missing,
        t.extra,
        inst_rules,
    );

    eprintln!("  {rel_lambda} release steps placed inside a lambda body");
    eprintln!(
        "  {} boundary crossings took the planned rung, {} took another          ({} of them terminal)",
        t.rungs_planned, t.rungs_unruled, t.rungs_terminal
    );
    for ((site, planned, took), n) in &ladder {
        eprintln!("    ladder {site:?}: plan {planned:?}, took {took:?} x{n}");
    }
    for (k, ex) in &unruled {
        let mut it = ex.iter();
        eprintln!(
            "    UNRULED {k:?} {} distinct pairs: {:?}",
            ex.len(),
            it.by_ref().take(12).collect::<Vec<_>>()
        );
    }
    eprintln!(
        "  of {} off-program answers, {} were given inside a lifted          lambda's cloned body and {} inside a cloned `where` predicate",
        t.synthesized, t.in_lambda, t.in_predicate
    );

    let mut top: Vec<_> = residue.into_iter().collect();
    top.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    let mut per_site: std::collections::BTreeMap<String, usize> = Default::default();
    for (k, n) in &top {
        let engine = k.split('/').next().unwrap();
        let suffix = match k.rsplit_once('~') {
            Some((_, tail)) => &k[k.len() - tail.len() - 1..],
            None => "",
        };
        *per_site.entry(format!("{engine}{suffix}")).or_insert(0) += n;
    }
    eprintln!("  the residue, by engine: {per_site:?}");
    // ...and by name, with an example to open.
    top.truncate(24);
    for (k, n) in &top {
        eprintln!("  residue {k}: {n}  (first: {})", residue_ex[k]);
    }
    let mut untop: Vec<_> = untyped.into_iter().collect();
    untop.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    untop.truncate(8);
    eprintln!("  the rows with no type, by engine and expression kind: {untop:?}");

    eprintln!(
        "  {} core right-hand sides typed by the emitter, {} differing from the checker under a rule",
        t.typed, t.typed_ruled
    );

    let mut report = String::new();
    if !typing_diffs.is_empty() {
        let lines: Vec<String> = typing_diffs
            .iter()
            .map(|((kind, what, rec, got), (n, first))| {
                format!("{n:5}x  {kind} {what}: checker `{rec}`, emitter `{got}`  (first: {first})")
            })
            .collect();
        report.push_str(&format!(
            "the emitter types {} core right-hand sides differently from the checker:
{}
",
            lines.len(),
            lines.join(
                "
"
            )
        ));
    }
    if !missing.is_empty() {
        let lines: Vec<String> = missing
            .into_iter()
            .map(|((site, k), ex)| format!("  {site:?} emitted `{k}`  (first: {ex})"))
            .collect();
        report.push_str(&format!(
            "a backend instantiated {} bodies the lowering's worklist does not \
             have:\n{}\n\nnote: the lowering is the worklist now. A body only a \
             backend knows about is a decision that is still in a backend.\n",
            lines.len(),
            lines.join("\n")
        ));
    }
    if !extra.is_empty() {
        let lines: Vec<String> = extra
            .into_iter()
            .map(|(k, ex)| format!("  `{k}`  (first: {ex})"))
            .collect();
        report.push_str(&format!(
            "the lowering built {} instances neither backend emitted, and no \
             rule explains them:\n{}\n",
            lines.len(),
            lines.join("\n")
        ));
    }
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
            "the lowered form failed its own lint:\n  {}\n",
            lint_failures.join("\n  ")
        ));
    }
    assert!(report.is_empty(), "{report}");

    // An answer about a walked node at an unbuilt substitution is an
    // instantiation the lowering is missing.
    assert_eq!(
        t.uninstantiated, 0,
        "{} backend answers are about a node the lowering recorded, under an \
         instantiation it did not build",
        t.uninstantiated
    );

    // A ceiling, not an equality: a synthesized node's address is a freed
    // temporary the allocator reuses, so collisions vary per run and per
    // allocator. The count follows the code the corpus links. A backend that
    // expands a projection for itself again, or clones a callee, fails here.
    assert!(
        t.synthesized < 1_400,
        "{} backend answers are about AST no instantiation of the program holds.          Without lambda and predicate clones it measured 1,052;          a number near 2,000 means one of them is back, near 3,300 that a          `place atSet` is expanded per engine again, and near 4,600 that the read          half is too",
        t.synthesized
    );

    // Each engine marks the rows it gives inside a tree it copied, so a clone
    // that comes back lands here rather than in the drifting count above.
    assert_eq!(
        t.in_lambda, 0,
        "{} backend answers were given inside a COPY of a lambda's body. The direct          backend queues the literal's own nodes (`Cx::lambdas`); a copy means it is          synthesizing a body again",
        t.in_lambda
    );
    assert_eq!(
        t.in_predicate, 0,
        "{} backend answers were given inside a COPY of a `where` predicate. Both          backends read the program's own predicate node; a copy means one of them is          walking `decl_map`'s again",
        t.in_predicate
    );

    // A `peek` question here means an emitter builds a source tree again.
    assert_eq!(
        t.peek_off, 0,
        "`peek` answered {} questions about AST no instantiation holds. The emitter \
         builds no source tree of its own since the AST walk was deleted",
        t.peek_off
    );

    // The terminal rung first: it is a program compiling on one target only.
    // Emitters ask the plan, so this fails only if one grows a rung of its own.
    assert_eq!(
        t.rungs_terminal, 0,
        "{} boundary crossings are at the end of one ladder and not the other —          see the UNRULED lines above",
        t.rungs_terminal
    );
    assert_eq!(
        t.rungs_unruled, 0,
        "{} boundary crossings took a rung the plan does not place — see the          UNRULED lines above",
        t.rungs_unruled
    );
    // A shadow that observes nothing asserts nothing.
    assert!(
        t.rungs_planned > 10_000,
        "only {} boundary crossings took the planned rung — the ladder shadow stopped          seeing the corpus",
        t.rungs_planned
    );

    // The typing witness grows as the core takes bodies. Measured 388,336.
    assert!(
        t.typed > 300_000,
        "only {} core right-hand sides were typed by the emitter — the typing          witness stopped seeing the corpus",
        t.typed
    );

    // The same floor for the instance comparison.
    assert!(
        t.extra > 0 && t.instances > 1_000,
        "the instance comparison stopped seeing the corpus: {} instances, {} \
         explained differences",
        t.instances,
        t.extra
    );
}

/// The plan at the pairs the corpus does not reach: the ends of the ladder, and
/// the rung order, which a green corpus cannot see.
#[test]
fn the_plan_places_the_rungs_the_two_ladders_were_read_at() {
    use vyrn_codegen::Rung as R;
    let decls: HashMap<String, TypeDecl> = HashMap::new();
    let plan = |a: &Type, b: &Type| vyrn_codegen::coerce_plan(a, b, &decls);
    let i8t = Type::IntN {
        bits: 8,
        signed: true,
    };
    let u8t = Type::IntN {
        bits: 8,
        signed: false,
    };
    // The one place the order is observable: a plan that put its shape shortcut
    // first would answer `Identity` for two integers of one width.
    assert_eq!(plan(&i8t, &u8t), R::Resize);
    assert_eq!(plan(&i8t, &i8t), R::Identity);
    assert_eq!(plan(&Type::Int, &Type::Float), R::FloatCross);
    assert_eq!(plan(&Type::Never, &Type::Str), R::Never);
    // The end: nothing in the ladder reconciles these.
    assert_eq!(plan(&Type::Str, &Type::Int), R::Refuse);
    assert_eq!(plan(&Type::Str, &Type::Param("T".into())), R::Refuse);
}

// The coercion census: one row per site that decides something about
// a coercion. The metric is code lines - non-blank and not a comment - over the
// site's whole span, doc comment included.

/// One site that decides something about a coercion.
struct CoercionSite {
    /// The file, under `compiler/`.
    file: &'static str,
    /// The signature line, matched on whitespace-collapsed text. It must name
    /// exactly one line of the file.
    at: &'static str,
    /// The engine that carries the decision, or `shared` for one statement all
    /// of them ask.
    engine: &'static str,
    /// Whether it is the rung ladder. The other rows are listed so a reader does
    /// not go looking for them.
    ladder: bool,
    /// Whether it states the rung rule, rather than emitting a rung another site
    /// placed.
    states_rung: bool,
    /// Its code lines.
    code: usize,
}

fn coercion_census() -> Vec<CoercionSite> {
    let site = |file, at, engine, ladder, states_rung, code| CoercionSite {
        file,
        at,
        engine,
        ladder,
        states_rung,
        code,
    };
    vec![
        site("vyrn-codegen/src/lib.rs", "pub fn coerce_plan(from: &Type, to: &Type, types: &HashMap<String, TypeDecl>) -> Rung {", "shared", true, true, 54),
        site("vyrn-codegen/src/direct.rs", "fn coerce(", "wasm", true, false, 178),
        site("vyrn-frontend/src/checker.rs", "fn prove_coercion(&self, expr: &Expr, to: &Type, line: usize) -> Result<(), Diagnostic> {", "checker", false, false, 26),
    ]
}

/// The span a site holds - `(first, last)`, one-based and inclusive, doc comment
/// included - and its code lines.
fn coercion_span(s: &CoercionSite) -> (usize, usize, usize) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(s.file);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", s.file));
    let text = text.replace("\r\n", "\n");
    let lines: Vec<&str> = text.lines().collect();
    let norm = |l: &str| l.split_whitespace().collect::<Vec<_>>().join(" ");
    let want = norm(s.at);
    let hits: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| norm(l) == want)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "the anchor `{}` names {} lines of {}; a census row's anchor must name one",
        s.at,
        hits.len(),
        s.file
    );
    let anchor = hits[0];
    let mut first = anchor;
    while first > 0 {
        let t = lines[first - 1].trim_start();
        if t.starts_with("//") || t.starts_with("#[") {
            first -= 1;
        } else {
            break;
        }
    }
    let (mut depth, mut open) = (0i32, false);
    let mut last = None;
    for (k, l) in lines.iter().enumerate().skip(anchor) {
        for ch in l.chars() {
            if ch == '{' {
                depth += 1;
                open = true;
            } else if ch == '}' {
                depth -= 1;
                if open && depth == 0 {
                    last = Some(k);
                    break;
                }
            }
        }
        if last.is_some() {
            break;
        }
    }
    let last = last.unwrap_or_else(|| panic!("no closing brace for `{}` in {}", s.at, s.file));
    let code = lines[first..=last]
        .iter()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with("//")
        })
        .count();
    (first + 1, last + 1, code)
}

/// Pins each site's code lines, so a change to a ladder moves this table.
#[test]
fn every_coercion_site_keeps_its_pinned_code_lines() {
    let census = coercion_census();
    // The engines' own ladder lines, without the shared plan they ask.
    let mut ladder = 0usize;
    for s in &census {
        let (_, _, code) = coercion_span(s);
        assert_eq!(
            code, s.code,
            "`{}` in {} is {code} code lines and the census says {}",
            s.at, s.file, s.code
        );
        if s.ladder && s.engine != "shared" {
            ladder += code;
        }
    }
    assert_eq!(
        ladder, 178,
        "the rung ladder is {ladder} code lines, not 178"
    );
    // An engine that asks another site's statement of the rung rule is not one.
    let statements: std::collections::BTreeSet<&str> = census
        .iter()
        .filter(|s| s.states_rung)
        .map(|s| s.engine)
        .collect();
    assert_eq!(
        statements.len(),
        1,
        "the rung rule is stated {} times and the census says 1: {statements:?}",
        statements.len()
    );
}
