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
//! **M6 shares and types the WRITING half, and it is 40% of what was left.**
//! `a[i] = v` expands a `place atSet` the same way `a[i]` expands a `place at`
//! — but its receiver is a NAME, so the expansion was keyed on a synthesized
//! `Expr::Var` sitting on the stack, and a stack address is not an identity.
//! Each engine therefore built its own tree, of the projection's body AND the
//! move-out/mutate/move-back group around the stored value. Keyed on the INDEX
//! node instead ([`vyrn_frontend::project::stored`]) it is one tree, the
//! checker types it, and [`Tally::synthesized`] is **3,266 -> 1,955** with
//! `peek`'s own share **501 -> 299**.
//!
//! **M3b types them, and the typing is the checker's own.** An expansion is
//! leaked, so its addresses are immortal and can be a key; and it is checked in
//! the CALLER's scope, which is the only place the answer is concrete —
//! `place at` on `Slots<T>` is checked once with `T` open, and no backend can
//! use a `T`. [`Tally::unrecorded`] is **4,707 → 78**, and the 78 are one named
//! class.
//!
//! **M4's third comparison was a SEQUENCE rather than an answer, and the half of
//! it that gated the two compiled backends has retired.** "Innermost frame
//! first, newest binding first" was asserted independently in three files
//! (§1.4); the shadow phases made all three engines report the sequence they
//! walk and gated it against the placement, over 78,371 walks. Both compiled
//! backends READ that placement now (`own::Ownership::releases`), so asserting
//! their walk against it is asserting a value equals itself, and the gate that
//! did it went with the derivation it was watching. What is left is
//! [`the_interpreter_releases_what_the_lowering_placed_in_the_order_it_placed_it`]
//! — the engine that still derives its own order is the one that still needs
//! gating, and it needs fixtures rather than the corpus because its walk happens
//! when a block RUNS.
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
    /// …of which this many were answered while the direct backend was lowering
    /// a LIFTED LAMBDA's body (RFC-0101 M6's second phase).
    ///
    /// M6's first phase ended by saying the residue's address is
    /// `Fn_::lift_lambda`'s clone and that "what is still owed before that
    /// phase starts is one measurement this one did not make: which of the 299
    /// are inside a lifted body and which are not". This is that measurement,
    /// kept rather than taken once.
    in_lambda: usize,
    /// …and this many while it was walking a `where` predicate — a tree BOTH
    /// backends clone twice over (`types::decl_map` copies the `TypeDecl`, then
    /// each validation site copies its predicate again). RFC-0101 has never
    /// named this clone; M6's second phase found it by measuring the class it
    /// expected to be the lambda's and finding the class was not there.
    in_predicate: usize,
    /// …and this many of the off-program answers were `peek`'s — the second
    /// expression typer the direct backend runs, and the number M3's delete half
    /// was priced on.
    ///
    /// It is the class RFC-0101 §2.3 leaves in the backend on purpose (the type
    /// of a release receiver, of a dispatched call the emitter builds at an emit
    /// site, and of the operands built beside them), so it has a FLOOR as well
    /// as a ceiling: 299 while the lambda and predicate clones were live, 109
    /// once both were measured, 110 once both were deleted.
    peek_off: usize,
    /// Instantiations one backend emitted that the lowering's worklist does not
    /// have. This is M2's gate and it is zero.
    missing: usize,
    /// …and the other direction, which is explained rather than zero: see
    /// [`InstRule`].
    extra: usize,
    unresolved: usize,
    /// RFC-0101 §1.5's shadow: boundary crossings that took the rung the plan
    /// places, and the ones that took another with no rule to explain it.
    rungs_planned: usize,
    rungs_unruled: usize,
    /// …and of those, the ones at the END of the ladder: a pair the plan refuses
    /// that an engine walked past anyway. This is the difference §1.5 says is
    /// the cheapest to gate and the most expensive to miss — the two ladders end
    /// differently, so a pair one of them does not handle is a compile error on
    /// exactly one target, or a reinterpretation of bits on the other.
    rungs_terminal: usize,
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
}

// `InstRule::ImplicitDispatch` was here and is RETIRED — RFC-0101 M5. It named
// the flattened protocol-impl method the SOURCE never calls: the `release` a
// scope exit reaches through `impl Owned for Slots<T>`. M2 wrote it, M4 measured
// it at 24 and said it "closes with the consumption"; the consumption landed and
// it stayed 24, because `own::release_kind` threw the receiver type away and a
// name alone does not say which instance a generic release reaches. The step
// carries the type now (`DropKind::Release(name, receiver)`) and `vyrn_lower`
// solves the instance from it, so the class is empty and the rule goes rather
// than firing zero times — §3 M2's own precedent, applied to itself for the
// second time.

/// Why the interpreter's release sequence differs from the placement both
/// compiled backends now read — RFC-0101 M4.
///
/// Two of the three rules retired with the compiled backends' derivations:
/// `Terminated` was a block that had already returned, and `StreamCursor` was
/// the one frame entry the placement has nothing for. Neither is reachable from
/// this engine. What is left is the difference §1.4 recorded and §2.4 calls a
/// DECLARED one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum RelRule {
    /// The interpreter acts on 2 of `own`'s 7 kinds (§1.4): a `String` or an
    /// array buffer is reclaimed by the host when the process ends, so it runs
    /// no step for one. What it DOES run is the two that a program can observe —
    /// a declared `release`, and the walk that reaches one.
    HostReclaims,
}

/// Which rule, if any, explains the interpreter's release walk against the
/// placement. `None` means the gate fails.
///
/// It is a SUBSEQUENCE test, which is the point: this engine may run fewer steps
/// than the form places, for a reason it can name, and it may never run them in
/// a different order. A reordering fits no rule and fails.
fn rel_rule(
    want: &[usize],
    got: &[usize],
    kind_of: &HashMap<usize, vyrn_frontend::own::DropKind>,
) -> Option<RelRule> {
    use vyrn_frontend::own::DropKind;
    // Order first, and unconditionally: whatever the engine skips, what it does
    // run is in the order the form placed it.
    let mut w = want.iter();
    if !got.iter().all(|g| w.any(|x| x == g)) {
        return None;
    }
    let skipped: Vec<&DropKind> = want
        .iter()
        .filter(|b| !got.contains(b))
        .filter_map(|b| kind_of.get(b))
        .collect();
    if skipped
        .iter()
        .all(|k| !matches!(k, DropKind::Release(..) | DropKind::Deep(_)))
    {
        return Some(RelRule::HostReclaims);
    }
    None
}

/// The placement, as a consumer reads it: every step the form put at one exit,
/// keyed by the node that exit is AT.
///
/// This is the keying RFC-0101 M4's second phase chose, and the reason is this
/// function: an engine standing at a `break` has the `Stmt::Break` in hand and
/// nothing else, and a reader that had to re-derive `LoopCtx::drop_boundary` to
/// find its steps would be the fourth copy of the thing being deleted.
fn placement(
    lowered: &vyrn_lower::Lowered,
) -> (
    HashMap<(vyrn_frontend::own::Exit, usize), Vec<usize>>,
    HashMap<usize, vyrn_frontend::own::DropKind>,
) {
    let mut placed: HashMap<(vyrn_frontend::own::Exit, usize), Vec<usize>> = HashMap::new();
    let mut kind_of: HashMap<usize, vyrn_frontend::own::DropKind> = HashMap::new();
    for inst in &lowered.instances {
        for rel in &inst.releases {
            // A generic body has one AST and many instances, so the sequence at
            // one exit is the same under every instantiation; it is held once.
            let seq = placed.entry((rel.exit, rel.site)).or_default();
            if !seq.contains(&rel.binding) {
                seq.push(rel.binding);
            }
            kind_of.insert(rel.binding, rel.kind.clone());
        }
    }
    (placed, kind_of)
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

/// The third engine's half of M4's shadow, and it needs a different shape.
///
/// The interpreter's release walk is not a static one: it happens when the
/// program RUNS, so reading it means running programs, and running the corpus
/// in-process is a different test from this file's. Fixtures instead, each the
/// smallest program that puts one exit kind to the walk: does it release the
/// same bindings the lowering placed, in the same order, and is everything it
/// skips a kind the host reclaims (§1.4 — the interpreter acts on 2 of `own`'s
/// 7).
///
/// The last column of the table is the exit kind the fixture EXISTS for, and it
/// is asserted to have actually been reached. A fixture whose shape stopped
/// producing the walk it was written for would otherwise pass by comparing
/// nothing, which is the failure mode a gate is for.
#[test]
fn the_interpreter_releases_what_the_lowering_placed_in_the_order_it_placed_it() {
    use vyrn_frontend::own::Exit;
    const OWNED: &str = "protocol Owned {\n    fn release(consume self)\n}\n\
                         type Ring = {\n    label: String\n}\n\
                         impl Owned for Ring {\n    fn release(consume self) {\n        \
                         print(self.label)\n    }\n}\n\
                         fn ring(l: String) -> Ring {\n    \
                         return Ring { label: l.copy() }\n}\n\
                         fn maybe() -> Option<Ring> {\n    return Some(ring(\"s\"))\n}\n\
                         fn none() -> Option<Int64> {\n    return None\n}\n\n";
    // The block exit's own fixtures keep every binding inside an `if`, and that
    // is not decoration: a function body ends in `return`, which is an EARLY
    // exit in all three engines, so `main`'s own block never runs a fall-through
    // walk. (It is why the corpus gate names its `Terminated` exits.)
    //
    // A declared owner between two `String`s: the two buffers are host-reclaimed
    // and the `Ring` is not, so the interpreter runs one of three placed steps.
    let mixed = format!(
        "{OWNED}fn main() -> Int64 {{\n    if true {{\n        let s = \"a\" + \"b\"\n        \
         let r = ring(\"x\")\n        let t = \"c\" + \"d\"\n        \
         print(s + t)\n    }}\n    return 0\n}}\n"
    );
    // Two owners, so the ORDER is what the fixture is about: newest first.
    let two = format!(
        "{OWNED}fn main() -> Int64 {{\n    if true {{\n        \
         let a = ring(\"a\")\n        let b = ring(\"b\")\n        print(\"in\")\n    }}\n    \
         return 0\n}}\n"
    );
    // A nested block, so "innermost frame first" is asserted too.
    let nested = format!(
        "{OWNED}fn main() -> Int64 {{\n    if true {{\n        \
         let a = ring(\"a\")\n        if true {{\n            \
         let b = ring(\"b\")\n            print(\"in\")\n        }}\n    }}\n    \
         return 0\n}}\n"
    );
    // `return` out of two frames at once: the walk is one sequence across both,
    // innermost first, and this engine produces it one frame at a time.
    let ret = format!(
        "{OWNED}fn f() -> Int64 {{\n    let outer = ring(\"o\")\n    if true {{\n        \
         let inner = ring(\"i\")\n        return 1\n    }}\n    return 0\n}}\n\
         fn main() -> Int64 {{\n    return f() - 1\n}}\n"
    );
    // `break` reaches the loop body's frames and stops there — the binding above
    // the loop is not on the walk, which is the boundary index asserted.
    let brk = format!(
        "{OWNED}fn f() -> Int64 {{\n    let outer = ring(\"o\")\n    let mut i = 0\n    \
         while i < 3 {{\n        let inLoop = ring(\"l\")\n        break\n    }}\n    \
         return 0\n}}\n\
         fn main() -> Int64 {{\n    return f()\n}}\n"
    );
    // `continue`, which runs the same frames once per turn.
    let cont = format!(
        "{OWNED}fn f() -> Int64 {{\n    let mut i = 0\n    while i < 2 {{\n        \
         let inLoop = ring(\"l\")\n        i = i + 1\n        continue\n    }}\n    \
         return 0\n}}\n\
         fn main() -> Int64 {{\n    return f()\n}}\n"
    );
    // A propagating `?`, which is a function exit and pays what one pays. This
    // is the walk RFC-0101 M4's step 0 found the interpreter was not running at
    // all, and `examples/releaseacrosstry.vyrn` is its parity pin.
    let tri = format!(
        "{OWNED}fn f() -> Option<Int64> {{\n    let r = ring(\"t\")\n    \
         let v = none()?\n    return Some(v)\n}}\n\
         fn main() -> Int64 {{\n    let a = f()\n    return 0\n}}\n"
    );
    // The temporary a `match` owns, and the handover beside it: the first has a
    // step because no arm took the scrutinee, the second has none because an arm
    // did and the binding it flowed into is the one owner there is.
    let scrut = format!(
        "{OWNED}fn f() -> Int64 {{\n    let n = match maybe() {{\n        \
         Some(r) => 1,\n        None => 0\n    }}\n    return n\n}}\n\
         fn main() -> Int64 {{\n    return f() - 1\n}}\n"
    );
    let handover = format!(
        "{OWNED}fn f() -> Int64 {{\n    if true {{\n        \
         let kept = match maybe() {{\n            Some(r) => r,\n            \
         None => ring(\"f\")\n        }}\n        print(\"held\")\n    }}\n    \
         return 0\n}}\n\
         fn main() -> Int64 {{\n    return f()\n}}\n"
    );

    for (what, src, reaches) in [
        ("mixed", &mixed, Exit::Block),
        ("two", &two, Exit::Block),
        ("nested", &nested, Exit::Block),
        ("return", &ret, Exit::Return),
        ("break", &brk, Exit::Break),
        ("continue", &cont, Exit::Continue),
        ("try", &tri, Exit::Try),
        ("scrutinee", &scrut, Exit::Scrutinee),
        ("handover", &handover, Exit::Block),
    ] {
        let mut program = vyrn_frontend::check(src).expect("the fixture checks");
        let diags = vyrn_frontend::check_and_synthesize(&mut program);
        assert!(diags.is_empty(), "{what}: {diags:?}");

        let lowered = vyrn_lower::lower(&program);
        let (placed, kind_of) = placement(&lowered);
        assert!(!placed.is_empty(), "{what}: the lowering placed nothing");

        vyrn_frontend::own::trace::start();
        let code = vyrn_frontend::interp::run(&program);
        let exits = vyrn_frontend::own::trace::take();
        assert_eq!(code, Ok(0), "{what}");
        assert!(!exits.is_empty(), "{what}: the interpreter walked no exit");

        let mut reached = false;
        let mut checked = 0;
        for e in &exits {
            let want = placed.get(&(e.exit, e.at)).cloned().unwrap_or_default();
            reached |= e.exit == reaches && !e.bindings.is_empty();
            checked += 1;
            if want == e.bindings {
                continue;
            }
            assert_eq!(
                rel_rule(&want, &e.bindings, &kind_of),
                Some(RelRule::HostReclaims),
                "{what}: the interpreter released {:?} at a {:?} exit, where the \
                 lowering placed {want:?}, and no rule explains it",
                e.bindings,
                e.exit
            );
        }
        assert!(checked > 0, "{what}: nothing was compared");
        assert!(
            reached,
            "{what}: the fixture is written for {reaches:?} and no release ran at \
             one — it would be asserting about a walk that never happened"
        );
    }
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
    let mut residue_ex: std::collections::BTreeMap<String, String> = Default::default();
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
    // RFC-0101 M4's phase-1 finding, still counted because it is the one fact
    // about the placement no engine reports: the corpus places no release step
    // inside a lambda body, so the shell both backends lower one under owns
    // nothing it has to unwind.
    let mut rel_lambda = 0usize;
    let mut inst_rules: std::collections::BTreeMap<InstRule, usize> = Default::default();
    // RFC-0101 §1.5's shadow: (engine, planned rung, rung taken) -> count, and
    // the ones no rule explains.
    let mut ladder: std::collections::BTreeMap<
        (Site, vyrn_codegen::Rung, vyrn_codegen::Rung),
        usize,
    > = Default::default();
    #[allow(clippy::type_complexity)]
    let mut unruled: std::collections::BTreeMap<
        (Site, vyrn_codegen::Rung, vyrn_codegen::Rung),
        std::collections::BTreeSet<String>,
    > = Default::default();
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
        // …and the `where` predicates, which are under no substitution for a
        // stronger reason: a predicate lives on a declaration and a declaration
        // has no type parameters. Every engine walks the same one at every
        // boundary the type crosses, INSIDE whatever body that boundary is in,
        // so an answer about one is looked up with the instantiation dropped.
        let mut predicate_nodes: std::collections::HashSet<usize> = Default::default();
        for row in &lowered.predicates {
            if let Node::Expr(e) = row.node {
                recorded.insert(
                    (row.node.id(), String::new()),
                    (row.ty.clone(), row.has.clone(), e),
                );
                walked.insert(row.node.id());
                predicate_nodes.insert(row.node.id());
            }
        }
        // The key an answer about `node` under `subst` is recorded at.
        let at = |node: usize, subst: &str| {
            if predicate_nodes.contains(&node) {
                (node, String::new())
            } else {
                (node, subst.to_string())
            }
        };

        // What the lowering says this program instantiates.
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
        let native = vyrn_codegen::emit(&program);
        let mut rows = observe::take();
        let mut insts = observe::take_insts();
        let mut crossings = observe::take_crossings();
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
        crossings.extend(observe::take_crossings());
        if wasm.is_err() {
            continue;
        }

        // RFC-0101 §1.5's shadow: every boundary crossing either engine made,
        // against the plan. The pair is the key — the two ladders reach `coerce`
        // from different call sites, so there is no node they both stand at.
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

        // Half zero, and RFC-0101 M2's own gate: the lowering's worklist against
        // each backend's. A body a backend emitted that the lowering does not
        // have is a hole in the lowering; a body the lowering has that a backend
        // does not is a target fact, and every one of them has to name its rule.
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
        let mut per_node: HashMap<(usize, String), Vec<(Site, Type, &'static str, &'static str)>> =
            HashMap::new();
        for row in &rows {
            per_node
                .entry(at(row.node, &subst_key(&row.subst)))
                .or_default()
                .push((row.site, row.ty.clone(), row.kind, row.ctx));
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
                    for (site, _, kind, ctx) in answers {
                        match *ctx {
                            "lambda" => t.in_lambda += 1,
                            "pred" => t.in_predicate += 1,
                            _ => {}
                        }
                        if *site == Site::Peek {
                            t.peek_off += 1;
                        }
                        // The `~` half is RFC-0101 M6's second phase: which
                        // CLONE the answer was given inside. Both are engine
                        // copies of a tree the program holds, so the split says
                        // how much of the residue a sharing move can reach and
                        // which move it is.
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

        // Half two: each backend answer against the recorded one.
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

    eprintln!("  RFC-0101 M4: {rel_lambda} release steps placed inside a lambda body");
    eprintln!(
        "  RFC-0101 §1.5: {} boundary crossings took the planned rung, {} took another          ({} of them terminal)",
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
        "  RFC-0101 M6: of {} off-program answers, {} were given inside a lifted          lambda's cloned body and {} inside a cloned `where` predicate",
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
    // …and by name, with an example to open. M5 measured this axis by editing
    // `observe::kind_of` by hand and RFC-0101 M6 was briefed from the result;
    // the edit is in the compiler now, so the ledger for the next milestone is
    // a test run rather than a patch.
    top.truncate(24);
    for (k, n) in &top {
        eprintln!("  residue {k}: {n}  (first: {})", residue_ex[k]);
    }
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
    // 9,505 -> 4,547 (M2c, by borrowing the callee's block) -> 3,294..3,484
    // (desugar-once, by expanding a `place at` projection ONCE for all three
    // walks) -> 1,955 (by sharing and typing the WRITING half as well) -> 1,052
    // here, by deleting the last two clones: the direct backend walks a lambda
    // literal's own body instead of a copy of it, and both backends read a
    // `where` predicate off the program instead of out of `decl_map`'s copy.
    // The ceiling is just above the highest of the runs measured: a backend
    // that goes back to expanding for itself, or to cloning a callee before it
    // lowers it, fails here.
    //
    // 2026-08-29: the ceiling moved 1,200 -> 1,400, and the reason is the
    // CORPUS, not an engine. The projection arc (RFC-0120..0122) and the
    // census closures added five examples and three std rule sets in one day,
    // and the count follows the code the corpus links — measured 1,208 on the
    // grown corpus, with the two zero-gates below (the actual clone
    // detectors) still at zero. The count is also address-collision noisy by
    // construction (the spread note above), which is what a same-tree Windows
    // CI failure against a green local run turned out to be: a different
    // allocator's collision pattern over a count already near the ceiling.
    assert!(
        t.synthesized < 1_400,
        "{} backend answers are about AST no instantiation of the program holds.          RFC-0101 M6 brought that to 1,052 by deleting the lambda and predicate          clones; a number near 2,000 means one of them is back, near 3,300 that a          `place atSet` is expanded per engine again, and near 4,600 that the read          half is too",
        t.synthesized
    );

    // The two clones, at zero, which is a stronger gate than a ceiling and the
    // reason the ceiling above could move. Each engine marks the rows it gives
    // inside a tree it COPIED; a copy's addresses are not the program's, so a
    // clone that comes back lands here rather than in a number that drifts.
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

    // …and the FLOOR, which is the other half of the same sentence. `peek`'s
    // share of the residue is the class RFC-0101 §2.3 assigns to the backend on
    // purpose: the type of a release receiver or of a dispatched call the
    // emitter builds at an emit site, which is a fact about a wasm local rather
    // than about a program. It was 299 while two clones were live and 109 once
    // both were measured away; it is not waiting for a mechanism, and a
    // milestone that drives it toward zero is moving a decision INTO the form
    // that §2.3 puts in the backend. Both bounds fail loudly rather than one:
    // a rise means a new engine-built tree, a fall means §2.3 moved.
    assert!(
        (90..=150).contains(&t.peek_off),
        "`peek` answered {} questions about AST no instantiation holds. RFC-0101 M6          measured this class at 109-110 and §2.3 owns every one of them; outside          90..150 the class has changed and the RFC's §2.3 leaves need re-reading",
        t.peek_off
    );

    // RFC-0101 §1.5's shadow, and the TERMINAL rung first, because it is the one
    // difference that is a program compiling on one target only: a pair one
    // ladder refuses and the other walks past. Both ladders ask the plan now
    // (RFC-0125 §3 M6, the coercion ladder), so the count is zero by
    // construction — and this is what says so out loud if an emitter grows a
    // rung of its own again.
    assert_eq!(
        t.rungs_terminal, 0,
        "{} boundary crossings are at the end of one ladder and not the other —          see the UNRULED lines above",
        t.rungs_terminal
    );
    // …and then the rest of the ladder. Every crossing takes the planned rung.
    // The four named differences RFC-0101 §1.5 recorded — an order, and a rung
    // one ladder did not have — went with the guards that produced them.
    assert_eq!(
        t.rungs_unruled, 0,
        "{} boundary crossings took a rung the plan does not place — see the          UNRULED lines above",
        t.rungs_unruled
    );
    // The floor under both: a shadow that observes nothing asserts nothing.
    assert!(
        t.rungs_planned > 10_000,
        "only {} boundary crossings took the planned rung — the ladder shadow stopped          seeing the corpus",
        t.rungs_planned
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

/// The plan itself, at the pairs the corpus does not reach — the ends of the
/// ladder, which is where the two engines used to disagree. It is stated once
/// and both emitters ask it now (RFC-0125 §3 M6), so what this pins is the
/// ORDER, which is the half of the rule a green corpus cannot see.
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
    // The one place the ORDER is observable: two integer spellings with one
    // shape. A plan that put its shape shortcut first would answer `Identity`
    // here and lose the only pair whose bits move.
    assert_eq!(plan(&i8t, &u8t), R::Resize);
    assert_eq!(plan(&i8t, &i8t), R::Identity);
    assert_eq!(plan(&Type::Int, &Type::Float), R::FloatCross);
    assert_eq!(plan(&Type::Never, &Type::Str), R::Never);
    // The END: nothing in the ladder reconciles these, and the two engines
    // disagree about what that means.
    assert_eq!(plan(&Type::Str, &Type::Int), R::Refuse);
    assert_eq!(plan(&Type::Str, &Type::Param("T".into())), R::Refuse);
}

// ---------------------------------------------------------------------------
// The coercion census — RFC-0125 §3 M6, the coercion ladder.
//
// §1 measures the ladder at "505 lines of one decision, and the two compiled
// backends order its rungs differently". §2.7 puts it on the deletion list.
// This is the list it is deleted from: one row per site that DECIDES something
// about a coercion, the engine that carries it, and its code lines.
//
// The metric is CODE lines — non-blank and not a comment — over the site's whole
// span, doc comment included. That is §1.1's own column, and it is what makes
// §1's 505 comparable: the six ladder rows measured 533 the day this census was
// written, and the difference is the observation hook RFC-0101 §1.5's shadow
// added after §1 was measured.
// ---------------------------------------------------------------------------

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
    /// What it decides.
    decides: &'static str,
    /// Whether it is the RUNG ladder — the code §1's 505 counts. The other rows
    /// are in the census so a later reader does not go looking for them.
    ladder: bool,
    /// Whether it STATES the rung rule, rather than emitting a rung another site
    /// placed. This is the column the milestone moves: an emitter that asks is
    /// not a statement (RFC-0125 §2.3).
    states_rung: bool,
    /// Its code lines, as the RFC records them.
    code: usize,
}

fn coercion_census() -> Vec<CoercionSite> {
    let site = |file, at, engine, decides, ladder, states_rung, code| CoercionSite {
        file,
        at,
        engine,
        decides,
        ladder,
        states_rung,
        code,
    };
    vec![
        site(
            "vyrn-codegen/src/lib.rs",
            "pub fn coerce_plan(from: &Type, to: &Type, types: &HashMap<String, TypeDecl>) -> Rung {",
            "shared",
            "which rung a pair takes",
            true,
            true,
            49,
        ),
        site(
            "vyrn-codegen/src/lib.rs",
            "fn coerce(&mut self, op: String, from: &Type, to: &Type) -> Result<(String, Type), String> {",
            "native",
            "the IR for the rung the plan placed",
            true,
            false,
            111,
        ),
        site(
            "vyrn-codegen/src/direct.rs",
            "fn coerce(",
            "wasm",
            "the wasm for the rung the plan placed",
            true,
            false,
            169,
        ),
        site(
            "vyrn-frontend/src/interp.rs",
            "fn coerce(&self, v: Val, ty: &Type) -> Result<Val, Ctrl> {",
            "interp",
            "the scalar targets that need no walk",
            true,
            true,
            19,
        ),
        site(
            "vyrn-frontend/src/interp.rs",
            "fn coerce_walk(&self, v: Val, ty: &Type) -> Result<Val, Ctrl> {",
            "interp",
            "the rung, by target type and value shape",
            true,
            true,
            112,
        ),
        site(
            "vyrn-frontend/src/interp.rs",
            "fn coercion_is_noop(&self, ty: &Type, v: &Val, depth: usize) -> bool {",
            "interp",
            "whether the walk would change the value",
            true,
            true,
            86,
        ),
        site(
            "vyrn-frontend/src/interp.rs",
            "fn coercion_is_identity(&self, ty: &Type, depth: usize) -> bool {",
            "interp",
            "whether a target type can change any value at all",
            true,
            true,
            35,
        ),
        site(
            "vyrn-codegen/src/lib.rs",
            "fn coerce_flow(",
            "native",
            "whether RFC-0020's containment proof skips the check",
            false,
            false,
            15,
        ),
        site(
            "vyrn-frontend/src/checker.rs",
            "fn prove_coercion(&self, expr: &Expr, to: &Type, line: usize) -> Result<(), Diagnostic> {",
            "checker",
            "whether a CONSTANT fails its target's predicate at compile time",
            false,
            false,
            44,
        ),
    ]
}

/// The span a site holds — `(first, last)`, one-based and inclusive, doc comment
/// included — and its code lines.
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

/// The census's line counts, as RFC-0125 §3 M6 records them. The prose quotes
/// these numbers, so they are asserted rather than described: a change to a
/// ladder moves one, and the RFC's table moves with it.
#[test]
fn the_coercion_census_is_what_the_rfc_records() {
    let census = coercion_census();
    // The ladder's CARRIERS — the engines' own statements — and, separately, the
    // one statement they can ask. §1's 505 is the first of the two.
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
        ladder, 532,
        "the rung ladder is {ladder} code lines and RFC-0125 §3 M6 records 532"
    );
    // The separate statements of the rung rule, which is what the milestone
    // moves: an engine that ASKS another site's statement is not one. It was
    // four — the two emitters, the interpreter, and a plan nobody asked.
    let statements: std::collections::BTreeSet<&str> = census
        .iter()
        .filter(|s| s.states_rung)
        .map(|s| s.engine)
        .collect();
    assert_eq!(
        statements.len(),
        2,
        "the rung rule is stated {} times and the census says 2: {statements:?}",
        statements.len()
    );
}

/// The table for RFC-0125 §3 M6, printed from the census above:
/// `cargo test -p vyrn-cli --test lowered -- --ignored --nocapture
/// the_coercion_census_as_a_table`.
#[test]
#[ignore]
fn the_coercion_census_as_a_table() {
    println!("| site | rung ladder | states the rung | engine | what it decides | code |");
    println!("|---|---|---|---|---|---|");
    for s in coercion_census() {
        let (a, b, code) = coercion_span(&s);
        let name = s.at.split_whitespace().collect::<Vec<_>>().join(" ");
        println!(
            "| `{}` {a}-{b} `{name}` | {} | {} | {} | {} | {code} |",
            s.file,
            if s.ladder { "yes" } else { "no" },
            if s.states_rung { "yes" } else { "no" },
            s.engine,
            s.decides
        );
    }
}
