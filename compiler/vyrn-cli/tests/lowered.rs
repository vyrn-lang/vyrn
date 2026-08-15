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
//! **M3 amended what "the recorded type" means, and 21,154 of those 22,321
//! differences were the wrong question ([A16]).** A node carries a PAIR: the
//! type the value HAS when the node's code has run, and the type it must END UP
//! as. `1` under an `Int32` destination has an `Int64` and ends up an `Int32`,
//! and the coercion between them is `coerce`'s whole job. A backend derives the
//! first; the checker answers the second. So the assertion here is now
//! **membership**: a backend's answer equals one member of the pair, or a rule
//! explains why not. What is left is 1,167, and its largest class is
//! [`Rule::LessSpecific`] — the native backend's own substitution keeping a type
//! parameter the recorded answer does not have, which is the class M3's delete
//! half removes rather than reconciles.
//!
//! It runs in-process rather than through `vyrn`, because the thing being
//! compared never reaches a process boundary: both backends' answers come out of
//! `vyrn_codegen::observe`, a sink that records what each emitter was about to
//! return anyway.
//!
//! **M2c halved the residue this gate reports.** 9,505 of the answers above were
//! about AST no instantiation of the program holds, and the attribution — a
//! backend clones the callee before it lowers a specialization — turned out to
//! name a clone that bought nothing. The direct backend borrows the program's
//! own block for a generic instance and for an RFC-0023 specialization now, so
//! about 5,000 of those answers land on nodes the lowering recorded and are
//! compared here for the first time. See [`Tally::synthesized`] for what is left
//! and why a ceiling rather than a zero.
//!
//! **The desugar-once milestone shares what is expanded rather than borrowing
//! what already exists.** A `place at` projection is inlined AT its access site,
//! so the nodes an engine walks there are nodes no one wrote: each engine built
//! its own copy, at its own addresses, after the lowering had run. They are one
//! tree now ([`vyrn_frontend::project::Memo`], opened per program below), the
//! lowering walks it too, and it moved 4,605 → 3,484 out of
//! [`Tally::synthesized`] and 526 → 4,707 into [`Tally::unrecorded`] — the form
//! HOLDING a node it had no answer for.
//!
//! **M3b types them, and the typing is the checker's own.** An expansion is
//! leaked, so its addresses are immortal and can be a key; and it is checked in
//! the CALLER's scope, which is the only place the answer is concrete —
//! `place at` on `Slots<T>` is checked once with `T` open, and no backend can
//! use a `T`. [`Tally::unrecorded`] is **4,707 → 78**, and the 78 are one named
//! class.
//!
//! **M2 added a second comparison over the same run: the instance LISTS.** Each
//! backend runs its own monomorphization worklist and nothing outside a backend
//! could see either, so "the lowering builds what the backends build" was a
//! claim with no gate. It is one now — set equality on `(callee, type
//! arguments)`, resolved through every alias so one instance has one spelling —
//! and the differences it leaves are named on [`InstRule`], the way [`Rule`]
//! names the type differences. What M2 measured there is in RFC-0101 §3 M2.

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

/// An instantiation, spelled so the lowering and both backends land on one
/// string: the callee's name and its type arguments, resolved through every
/// declared alias.
///
/// [`deep`] rather than the type as written, because the three lists reach the
/// same instance by different routes — a backend substitutes through its own
/// `subst` and the lowering through the checker's recorded solution, so `Age`
/// and `Int64` are one instance under two spellings. It is deliberately NOT
/// `mangle_name`: that is the string defect #165 was, and two records mangle
/// alike.
fn inst_key(name: &str, args: &[Type], decls: &HashMap<String, TypeDecl>) -> String {
    if args.is_empty() {
        return name.to_string();
    }
    let args: Vec<String> = args.iter().map(|a| deep(a, decls, 0).to_string()).collect();
    format!("{name}<{}>", args.join(", "))
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
    /// element type of `[]`, the unused side of a `Result`. The value is coerced
    /// immediately afterwards, which is why no program has ever noticed.
    ///
    /// This was 21,148 before the form carried the pair and it is 2,114 after:
    /// almost all of it was a backend answering the has-type at a node whose
    /// destination the checker had already applied, which is a different
    /// question and not a difference. What is left is a position the form's
    /// has-derivation does not settle — a `match` arm's, a wasm local's.
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
    /// …of which this many answered the OTHER member of the pair: the type the
    /// value HAS, where the recorded one is the type it must END UP as ([A16]).
    answered_has: usize,
    /// …and this many equalled neither member.
    differed: usize,
    /// Answers where the two backends did not agree with EACH OTHER.
    cross_differed: usize,
    /// Backend answers whose node the checker never typed — a hole in the
    /// recording, counted not hidden.
    ///
    /// 526 → 4,707 with the desugar-once milestone, and the rise was the point:
    /// those are the expansion nodes, which the form holds and the checker had
    /// not seen. A row with no type is a place a type can go; no row at all is
    /// not.
    ///
    /// **M3b put the types in, and it is 78.** The checker types each expansion
    /// where it is inlined (`Checker::record_desugar`) and the loader's stamped
    /// `panic` site — 494 of the old count — is typed too. The 78 that remain
    /// are one class, printed by the run: a `Var` the checker resolves by NAME
    /// rather than by node, because the position must be a binding — the
    /// receiver of `xs.pop()` / `xs.swapRemove(i)`, and the place temporaries
    /// `parser::place_receiver` hoists (`s.free[]`). Closing it means the
    /// checker routing those through `Checker::expr`, which is a change to what
    /// a mutating builtin accepts and not a recording fix.
    unrecorded: usize,
    /// Backend answers about a node the lowering DID record, under an
    /// instantiation it did not build. This is the residue a worklist can close,
    /// and M2 closed it: module-state initializers are a root now.
    uninstantiated: usize,
    /// Backend answers about a node the lowering never recorded at all — a body
    /// the backend SYNTHESIZED (a lifted lambda's block, a specialization's
    /// substituted shell) or one no instantiation reaches.
    ///
    /// M1 counted this together with the row above and called the sum "answers
    /// inside an instantiation M1 does not build". M2 measured the split and it
    /// is 9,355 to 7: almost none of it is a missing instantiation. A backend
    /// clones the AST before it lowers a specialization, so the addresses are
    /// the clone's and no worklist can make them match.
    ///
    /// **M2c halved it by deleting the clone rather than by moving the body.**
    /// The direct backend deep-copied the callee for every generic instantiation
    /// and every RFC-0023 specialization; it borrows the program's own block for
    /// both now, so those answers land on nodes the lowering recorded. 9,505 →
    /// 4,547 (4,530 on the next run — see the ceiling below for why the count
    /// drifts), and that ceiling is what stops the clone coming back. What is
    /// left
    /// is AST no backend has anything to borrow: a lifted lambda's synthesized
    /// block and the desugars both backends build on the stack.
    ///
    /// **The desugar-once milestone took the second half of that sentence and
    /// found it was a third of what was left.** A `place` projection is
    /// expanded once now — `project::Memo`, opened around this compile — and
    /// the lowering walks the same tree the two backends do, so those nodes are
    /// the form's. 4,605 → 3,294..3,484. The rest is not a desugar and never
    /// was: `Wasm/var` alone is 1,458 of it, and it is the receiver a backend
    /// builds on the stack to reach an implicitly dispatched `release`,
    /// `size` or `success` — the `ImplicitDispatch` class M2 already named, and
    /// M4's to close.
    ///
    /// **M3b measured what that class costs the deletion, per engine.** Of the
    /// answers about a node the form holds, 264,908 native / 263,487 wasm /
    /// 52,182 `peek` are compared here; 1,678 / 2,985 / **501** are not. The
    /// 501 is the number M3's delete half is priced on: a `peek` that must
    /// still answer 501 questions cannot be deleted, and a lookup added beside
    /// it is the second type mechanism RFC-0101 §1.2 exists to remove.
    synthesized: usize,
    /// Instantiations one backend emitted that the lowering's worklist does not
    /// have. This is M2's gate and it is zero.
    missing: usize,
    /// …and the other direction, which is explained rather than zero: see
    /// [`InstRule`].
    extra: usize,
    unresolved: usize,
}

/// Why the lowering's instance list and a backend's differ.
///
/// Same discipline as [`Rule`]: a difference that fits a rule is a fact this
/// file records, and one that fits none fails the run. Both of these are the
/// same sentence — a backend emits fewer bodies than the program has, and each
/// omission is a TARGET fact (RFC-0101 §2.3), not a decision about the language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum InstRule {
    /// A `gen fn` (RFC-0021) runs in the compiler's interpreter at generation
    /// time and is never called in a shipped binary, so neither backend emits a
    /// body for it. The lowering has one, because the program does.
    GenFn,
    /// A flattened protocol-impl method the SOURCE never calls: the `release` a
    /// scope exit reaches through `impl Owned for Slots<T>`, the `size` a
    /// `for x in s` reaches through `impl Iterate`. The lowering's worklist
    /// follows the calls the program writes; these are calls the LANGUAGE
    /// writes, placed by the release walk and by the loop lowering.
    ///
    /// This is the only class in either direction that goes the wrong way — a
    /// backend has an instance the lowering does not — and it is deliberately
    /// not closed here. Guessing it ahead of the pass that PLACES the release
    /// would be a second source of truth about where a release happens, which is
    /// the failure mode `direct.rs`'s own worklist comment warns about. RFC-0101
    /// M4 puts the release steps in the form; the instantiation then comes from
    /// the step, and this rule goes with it.
    ImplicitDispatch,
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
    // The residue, by the engine that answered and the kind of expression —
    // the axis RFC-0101 §3 M2c classified by hand and this milestone re-measured
    // against. It is reported, never asserted: the raw count is not reproducible
    // (see [`Tally::synthesized`]), and the shape is what the next milestone is
    // briefed from.
    let mut residue: std::collections::BTreeMap<String, usize> = Default::default();
    // …and the rows the form HOLDS and has no type for, on the same axis. This
    // is the half M3 moved: the checker types an expansion now, so what is left
    // here is a node class the checker itself never routes through
    // `Checker::expr`.
    let mut untyped: std::collections::BTreeMap<String, usize> = Default::default();
    // (site, expression kind, recorded, backend) -> (count, first sighting)
    let mut disagreements: HashMap<(Site, &'static str, String, String), (usize, String)> =
        HashMap::new();
    let mut lint_failures: Vec<String> = Vec::new();
    // (engine A, engine B, expression kind, A's answer, B's answer) -> (count, example)
    let mut cross: HashMap<(Site, Site, &'static str, String, String), (usize, String)> =
        HashMap::new();
    let mut rules: std::collections::BTreeMap<Rule, usize> = Default::default();
    let mut inst_rules: std::collections::BTreeMap<InstRule, usize> = Default::default();
    // An instantiation a backend emitted and the lowering's worklist does not
    // have: (site, spelled instance) -> example.
    let mut missing: std::collections::BTreeMap<(Site, String), String> = Default::default();
    // …and one the lowering has that no rule explains away.
    let mut extra: std::collections::BTreeMap<String, String> = Default::default();

    for path in corpus() {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        t.examples += 1;
        let Ok(program) = load(&path) else {
            // A corpus entry that does not link (an expected check failure, a
            // generator needing a cache) is not this gate's subject.
            t.unloadable += 1;
            continue;
        };

        // One expansion per access site, shared by the lowering and both
        // backends for as long as this program is the one being compiled
        // (RFC-0101's desugar-once milestone). Without it each engine expands
        // for itself and the three walks land on three sets of addresses, which
        // is what `Tally::synthesized` counted.
        let _memo = vyrn_frontend::project::Memo::open();
        let decls = decl_map(&program);
        let lowered = vyrn_lower::lower(&program);
        for problem in vyrn_lower::lint(&lowered) {
            lint_failures.push(format!("{name}: {problem}"));
        }
        t.instances += lowered.instances.len();
        t.rows += lowered.rows();
        // A call the worklist stopped following is a gate, not a count: the only
        // legal reason is the monomorphization bound, which is the bound WORKING
        // (`examples/polyrecursion.vyrn` reached 18 GiB without it). An unsolved
        // parameter or an unknown callee would be a hole.
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

        // (node address, instantiation) -> the recorded type, and the node
        // itself so a disagreement can say what kind of expression it was.
        let mut recorded: HashMap<(usize, String), (Option<Type>, Option<Type>, &Expr)> =
            HashMap::new();
        // Every node address the lowering recorded, under ANY instantiation —
        // which is what separates "a body the lowering walked, at a substitution
        // it did not build" from "AST that is not in the program".
        let mut walked: std::collections::HashSet<usize> = Default::default();
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
                    recorded.insert(
                        (row.node.id(), key.clone()),
                        (row.ty.clone(), row.has.clone(), e),
                    );
                    walked.insert(row.node.id());
                }
            }
        }
        // The module-state initializers, which both backends lower inside a
        // synthesized function under no substitution at all.
        for row in &lowered.globals {
            if let Node::Expr(e) = row.node {
                recorded.insert(
                    (row.node.id(), String::new()),
                    (row.ty.clone(), row.has.clone(), e),
                );
                walked.insert(row.node.id());
            }
        }

        // What the lowering says this program instantiates.
        let mut lowering: std::collections::BTreeSet<String> = Default::default();
        for inst in &lowered.instances {
            lowering.insert(inst_key(&inst.func.name, &inst.type_args, &decls));
        }

        observe::start();
        let native = vyrn_codegen::emit(&program);
        let mut rows = observe::take();
        let mut insts = observe::take_insts();
        if native.is_err() {
            // A program the native backend refuses is not a program this gate
            // can compare; the wasm column would be answering about a different
            // walk. Parity already owns that failure.
            continue;
        }
        observe::start();
        let wasm = vyrn_codegen::direct::compile(&program);
        rows.extend(observe::take());
        insts.extend(observe::take_insts());
        if wasm.is_err() {
            continue;
        }

        // Half zero, and RFC-0101 M2's own gate: the lowering's worklist against
        // each backend's. A body a backend emitted that the lowering does not
        // have is a hole in the lowering; a body the lowering has that a backend
        // does not is a target fact, and every one of them has to name its rule.
        let mut backend: std::collections::BTreeSet<(Site, String)> = Default::default();
        // Every flattened protocol-impl method this program declares, by name.
        let impl_methods: std::collections::HashSet<String> = program
            .impls
            .iter()
            .filter_map(|i| vyrn_frontend::types::type_key(&i.ty).map(|k| (i.protocol.clone(), k)))
            .flat_map(|(p, k)| {
                program
                    .functions
                    .iter()
                    .map(|f| f.name.clone())
                    .filter(move |n| n.starts_with(&format!("{p}__{k}__")))
            })
            .collect();
        for i in &insts {
            let k = inst_key(&i.name, &i.args, &decls);
            if !lowering.contains(&k) {
                if impl_methods.contains(&i.name) {
                    *inst_rules.entry(InstRule::ImplicitDispatch).or_insert(0) += 1;
                } else {
                    t.missing += 1;
                    missing.entry((i.site, k.clone())).or_insert(name.clone());
                }
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
            // Deliberately no rule for the higher-order shell, and that is a
            // measurement rather than an omission: a backend skips the
            // first-order definition of a function with a `fn` parameter and
            // emits specializations instead, but a specialization keys back to
            // the SAME (callee, type arguments) the lowering built, so the two
            // lists agree without one. A shell that ever shows up here is a real
            // difference, and it fails.
            match by_name.get(f) {
                Some(f) if f.is_gen => *inst_rules.entry(InstRule::GenFn).or_insert(0) += 1,
                _ => {
                    extra.entry(k.clone()).or_insert(name.clone());
                }
            }
        }

        // Half one: the two compiled backends against EACH OTHER. This is the
        // sentence RFC-0101 §1.1 says nothing checks — "the two copies agree" —
        // and it needs no interpretation to gate.
        let mut per_node: HashMap<(usize, String), Vec<(Site, Type, &'static str)>> =
            HashMap::new();
        for row in &rows {
            per_node
                .entry((row.node, subst_key(&row.subst)))
                .or_default()
                .push((row.site, row.ty.clone(), row.kind));
        }
        for (key, answers) in &per_node {
            // Only nodes the lowering recorded. A backend also types AST it
            // builds itself — a lifted lambda's synthesized body, a desugared
            // method call — and those live in temporaries whose addresses are
            // reused, so two of them can collide on one key. A node of the
            // PROGRAM is alive for the whole compile and cannot be aliased.
            let Some((_, _, node)) = recorded.get(key) else {
                if walked.contains(&key.0) {
                    t.uninstantiated += 1;
                } else {
                    t.synthesized += 1;
                    for (site, _, kind) in answers {
                        *residue.entry(format!("{site:?}/{kind}")).or_insert(0) += 1;
                    }
                }
                continue;
            };
            let Some((_, first, _)) = answers.first() else {
                continue;
            };
            for (site, ty, _) in answers.iter().skip(1) {
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
            let Some((rec, has, node)) = recorded.get(&(row.node, subst_key(&row.subst))) else {
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
            // [A16]: the form carries a PAIR — what the value has and what it
            // must end up as — and a backend answering the other member is not a
            // disagreement, it is the other question. This is the assertion M3
            // exists to make: a backend's answer is one member of the pair, or a
            // rule below says why not.
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
        "RFC-0101 M1/M2 corpus gate: {} examples ({} did not link), {} instances, \
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

    let mut top: Vec<_> = residue.into_iter().collect();
    top.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    top.truncate(8);
    eprintln!("  the residue, by engine and expression kind: {top:?}");
    let mut untop: Vec<_> = untyped.into_iter().collect();
    untop.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    untop.truncate(8);
    eprintln!("  the rows with no type, by engine and expression kind: {untop:?}");

    let mut report = String::new();
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
            "the lowered form failed its own lint (RFC-0101 §2.6):\n  {}\n",
            lint_failures.join("\n  ")
        ));
    }
    assert!(report.is_empty(), "{report}");

    // M2's residue gate. A backend answer about a node the lowering WALKED, at a
    // substitution it did not build, is an instantiation the lowering is missing
    // — and there are none. (The other residue, about AST the backend
    // synthesized, is reported above and is not a worklist's to close.)
    assert_eq!(
        t.uninstantiated, 0,
        "{} backend answers are about a node the lowering recorded, under an \
         instantiation it did not build",
        t.uninstantiated
    );

    // M2c's gate, lowered by the desugar-once milestone. A backend answer about
    // AST the backend built itself can never read a recorded type, so this
    // number is what M3's delete half is blocked by, and a ceiling is the only
    // shape it can have: the raw count is not reproducible run to run (a
    // synthesized node's address is a freed temporary the allocator hands out
    // again, so two of them collide — or do not — per run), and M2 measured that
    // spread at about ±200 on 9,500.
    //
    // 9,505 -> 4,547 (M2c, by borrowing the callee's block) -> 3,294..3,484 here,
    // by expanding a `place` projection ONCE for all three walks. The ceiling is
    // just above the highest of the runs measured: a backend that goes back to
    // expanding for itself, or to cloning a callee before it lowers it, fails
    // here. What the number is now is NOT mostly desugars — see the residue line
    // the run prints, which is `var`, `field` and `call` under both engines: the
    // receivers a backend synthesizes to reach an implicitly dispatched
    // `release`, `size` or `success`, and the body of a lifted lambda. The first
    // class is M4's (the release steps move into the form); the second is the
    // milestone that moves lambda lifting.
    assert!(
        t.synthesized < 3_500,
        "{} backend answers are about AST no instantiation of the program holds.          The desugar-once milestone brought that to 3,484 by expanding each          `place` projection once and handing the same nodes to the lowering and          to both backends, and M3 to 3,218 by typing those nodes; a number back          near 4,600 means an engine is expanding for itself again",
        t.synthesized
    );

    // A gate that compares nothing passes trivially. This is the floor the run
    // above cleared by two orders of magnitude; it exists so a refactor that
    // quietly stops recording fails here rather than passing.
    assert!(
        t.compared > 10_000,
        "only {} backend answers were compared — the gate stopped seeing the corpus",
        t.compared
    );
    // …and the same floor for the pair's second member. The has-type is 21,154
    // of the corpus's answers; a change that quietly stopped deriving it would
    // otherwise read as green, with every one of those falling back to a rule.
    assert!(
        t.answered_has > 10_000,
        "only {} backend answers matched the pair's has-type — the form stopped          carrying it",
        t.answered_has
    );
    // …and the same floor for the instance comparison, so a hook that stops
    // firing reads as green instead of as a missing list.
    assert!(
        t.extra > 0 && t.instances > 1_000,
        "the instance comparison stopped seeing the corpus: {} instances, {} \
         explained differences",
        t.instances,
        t.extra
    );
}
