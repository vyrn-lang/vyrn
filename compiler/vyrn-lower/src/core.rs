//! The named core: every intermediate value has a name, every
//! access is a place, and every release the ownership plan decided is an
//! explicit [`St::Drop`]. The kernel (`kernel.rs`) checks over it that every
//! owned name is consumed exactly once on every path.
//!
//! The core takes three inputs it does not derive: the checker's type for
//! every expression ([`crate::NodeTypes`]), the plan's decisions
//! ([`vyrn_frontend::own::ReleasePlan`] and the placed [`Release`] rows), and
//! [`vyrn_frontend::declared::Owned`]. Where the plan placed a release a `Drop`
//! stands; where it did not, nothing stands, and the kernel decides whether
//! that is a leak. A construct this pass does not lower returns a [`Gap`], and
//! the instance counts as unlowered.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use vyrn_frontend::ast::{
    ArmBody, At, BinOp, Binder, Block, Capability, Expr, FnId, Function, Id, LambdaBody, MatchArm,
    MethodId, MethodSig, NodeId, Pattern, Program, SourceBody, Speech, Step, Stmt, Type, TypeDecl,
    UnOp,
};
use vyrn_frontend::declared::{CapsOf, NameMemo, Owned};
use vyrn_frontend::diagnostics::{Diagnostic, Fix};
use vyrn_frontend::effects::Walked;
use vyrn_frontend::movecheck::{Judgment, JudgmentKey, Refusal};
use vyrn_frontend::own::{Bucket, DropKind, Exit, Linear, MemoryRow, Ownership, Release};
use vyrn_frontend::prelude;
pub use vyrn_frontend::prelude::Spec;
use vyrn_frontend::project::is_place_read;

use crate::kernel::{MissingKind, Root};
use crate::world::{Fns, Stated};
use crate::{Instance, NodeTypes, OutsideBody, World};
use vyrn_frontend::core::{
    count_reads, names_in, rows, Arg, Arm, Body, BorrowKind, Callee, Cand, Copied, Ctor, Facts,
    Lit, Name, NameInfo, NotOwned, Old, Op, Opaque, Payload, Place, Rhs, Site, St, Target, Test,
    Use, Val, Walk,
};
use vyrn_frontend::rule;
use vyrn_frontend::rules::Rule;

/// The path a field read takes out of its unnamed receiver: `.q.s` for
/// `mk().q.s`. `None` under an element. A `forced` read is a receiver of its
/// own ([`Builder::place`]): `.s` for `h.f.s` where `f` is `lazy`.
fn taken_path(e: &Expr, forced: &impl Fn(&Expr) -> bool) -> Option<String> {
    match e {
        Expr::Field { expr, field, .. } if !forced(e) => {
            Some(format!("{}.{field}", taken_path(expr, forced)?))
        }
        Expr::Call { name, .. } if name == vyrn_frontend::project::AT => None,
        _ => Some(String::new()),
    }
}

/// The borrow a parameter's capability makes; `None` for `consume`.
/// `lambda` marks a lambda's parameter.
fn param_borrow(cap: Capability, name: &str, lambda: bool) -> Option<BorrowKind> {
    let cap = match cap {
        Capability::Read => "read",
        Capability::Modify => "modify",
        Capability::Consume => return None,
    };
    Some(BorrowKind::Param {
        cap,
        of: name.to_string(),
        lambda,
    })
}

/// The literal a literal expression is; `None` for any other expression.
/// This is the one statement of which forms are literals.
/// [`Builder::rhs_inner`] also names the five because its match is
/// exhaustive on purpose, and asks here for the answer.
fn lit_of(e: &Expr) -> Option<Lit> {
    Some(match e {
        Expr::Int(v, _) => Lit::Int(*v),
        Expr::Byte(v, _) => Lit::Byte(*v),
        Expr::Float(v, _) => Lit::Float(*v),
        Expr::Bool(v, _) => Lit::Bool(*v),
        Expr::Str(s, _) => Lit::Str(s.clone()),
        _ => return None,
    })
}

/// Calls `f` on every row of `ss` and every row under it, in program order,
/// each before the rows it holds.
fn each_row_mut(ss: &mut [St], f: &mut dyn FnMut(&mut St)) {
    for s in ss {
        f(s);
        s.lists_mut().for_each(|l| each_row_mut(l, f));
    }
}

/// A construct this pass does not lower. The instance is neither accepted nor
/// refused; the corpus test counts these by `what`.
#[derive(Debug, Clone)]
pub struct Gap {
    pub what: &'static str,
    /// A callee's or a binding's name; empty when `what` says it all.
    pub detail: String,
    pub line: usize,
    /// Set when this is a rule the program breaks rather than a gap: the
    /// refusal, which the placer reports as it reports the kernel's. A rule
    /// about a keyword lives here because the kernel has none
    /// (`consume make()` and `make()` are one value to it).
    pub rule: Option<Box<Diagnostic>>,
}

/// Whether `rhs` is a validated type's constructor over a literal, which
/// hands the literal back.
fn over_a_literal(rhs: &Rhs) -> bool {
    matches!(rhs, Rhs::Call { kind: Callee::Named | Callee::Proven, args, .. }
        if matches!(args.as_slice(), [(Arg::Val(Val::Lit(l)), _)] if !matches!(l, Lit::Opaque(_))))
}

/// A field read or an element read, whatever its receiver: the reads whose
/// receiver [`Builder::place`] binds to a temporary when it names no place.
fn reads_a_part(e: &Expr) -> bool {
    match e {
        Expr::Field { .. } => true,
        Expr::Call { name, args, .. } => name == "@at" && args.len() == 2,
        _ => false,
    }
}

/// Refuses a `consume` whose operand names no place. `by_loop` picks the
/// wording for `for x in consume xs` over a prefix `consume`
/// (`movecheck::TakeForm`). Both rules are syntactic, so they hold for
/// heapless types too.
fn take_names_a_place(
    e: &Expr,
    places: &HashSet<String>,
    line: usize,
    by_loop: bool,
) -> Result<(), Gap> {
    if vyrn_frontend::ast::place_path(e).is_some() {
        return Ok(());
    }
    if let Some((_, path)) = vyrn_frontend::project::element_path(e, places) {
        let container = vyrn_frontend::project::element_container(&path);
        let more = vec![rule!(SwapRemove, container).render()];
        return refuse(rule!(ElementTaken, path), more, line);
    }
    let rule = match by_loop {
        true => rule!(LoopTakesNothing),
        false => rule!(ConsumeTakesNothing),
    };
    refuse(rule, Vec::new(), line)
}

/// The scrutinee a binder borrows: its name, where the construct does not
/// own it. `None` where it does, whose payloads are the construct's to give.
fn borrow_root(sv: &Val, owns: bool) -> Option<Name> {
    match sv {
        Val::Name(n) if !owns => Some(*n),
        _ => None,
    }
}

/// The sites of the candidate constructs that are their value's last owner,
/// decided over the first build's core.
///
/// A construct takes its value where nothing reads the name after it (a
/// payload binder is a name of its own) and where the binding and the
/// construct stand under the same loops, so one value is not taken twice. A
/// [`Cand::Switch`] takes where the name's last read is the switch's own; a
/// [`Cand::Loop`] where the last read is deeper than the binding, so inside
/// the loop.
///
/// A release row is not a read. The rows are derived from the take; letting
/// a row decide the take made the answer depend on rows the placer had just
/// added, and the second build then seeded a different take.
fn last_owner(top: &Body) -> std::collections::HashSet<NodeId> {
    let mut out = std::collections::HashSet::new();
    for f in top.frames() {
        if f.cands.is_empty() {
            continue;
        }
        let mut w = Reads {
            last: vec![0; f.names.len()],
            deep: vec![0; f.names.len()],
            bound: vec![usize::MAX; f.names.len()],
            handed: vec![false; f.names.len()],
            switches: Vec::new(),
            order: 0,
        };
        w.stmts(&f.stmts, 0);
        for p in &f.params {
            w.bound[p.index()] = 0;
        }
        for (site, n, kind) in &f.cands {
            let takes = match kind {
                Cand::Switch => {
                    let at = w.switches.iter().find(|(s, m, _, _)| s == site && m == n);
                    at.is_some_and(|(_, _, depth, order)| {
                        w.last[n.index()] == *order && w.bound[n.index()] == *depth
                    })
                }
                Cand::Loop => {
                    w.bound[n.index()] != usize::MAX && w.deep[n.index()] > w.bound[n.index()]
                }
                Cand::Elem => w.handed[n.index()],
            };
            if takes {
                out.insert(*site);
            }
        }
    }
    out
}

/// The name a place is rooted at, as a value. `None` for module state.
fn root_name(p: &Place) -> Option<Val> {
    match p {
        Place::Name(n) => Some(Val::Name(*n)),
        Place::Global(_) => None,
        Place::Field(b, _) | Place::Elem(b, _) | Place::Key(b, _) => root_name(b),
    }
}

/// One frame's last read of each name, the loop depth that read stood at,
/// the loop depth each name was bound at, and every switch over a bare name
/// with its depth and the order of its own read. See [`last_owner`].
struct Reads {
    last: Vec<usize>,
    deep: Vec<usize>,
    bound: Vec<usize>,
    /// Whether the name was handed on rather than only read ([`Cand::Elem`]).
    handed: Vec<bool>,
    switches: Vec<(NodeId, Name, usize, usize)>,
    order: usize,
}

impl Reads {
    fn stmts(&mut self, stmts: &[St], depth: usize) {
        for st in stmts {
            // A row is the plan's, not the core's: see [`last_owner`].
            if matches!(st, St::Row { .. }) {
                continue;
            }
            st.operands(&mut |v, u| {
                let Val::Name(n) = *v else { return };
                let n = n.index();
                if u == Use::Bind {
                    self.bound[n] = depth;
                    return;
                }
                self.order += 1;
                self.last[n] = self.order;
                self.deep[n] = depth;
                self.handed[n] |= matches!(u, Use::Hand | Use::Key);
            });
            // A take out of a sub-place hands that part on, so what is left
            // is this turn's: `out.push(consume p.value)` in a `for p in ..`.
            if let St::Let(_, Rhs::Take(p))
            | St::Do {
                rhs: Rhs::Take(p), ..
            } = st
            {
                if let Some(Val::Name(n)) = root_name(p) {
                    self.handed[n.index()] = true;
                }
            }
            let St::Switch { on, arms, .. } = st else {
                let inner = depth + usize::from(matches!(st, St::Loop { .. }));
                st.lists().for_each(|l| self.stmts(l, inner));
                continue;
            };
            if let (Val::Name(n), Some(a)) = (on, arms.first()) {
                self.switches.push((a.site, *n, depth, self.order));
            }
            for a in arms {
                for b in &a.binds {
                    self.bound[b.index()] = depth;
                }
                // The rows that read a binder out of the scrutinee
                // are the binder's, not a read of the scrutinee.
                self.stmts(&a.body[a.reads(on).len()..], depth);
            }
        }
    }
}

/// A `match`'s head line and the last line an arm's value starts on, for
/// [`Builder::takes_scrutinee`]. A block arm yields no value and
/// adds no line.
fn arms_span(line: usize, arms: &[MatchArm]) -> (usize, usize) {
    let last = arms.iter().fold(line, |m, a| match &a.body {
        ArmBody::Expr(e) => m.max(e.line()),
        ArmBody::Block(_) => m,
    });
    (line, last)
}

/// The kind of an expression, for a gap's detail: [`crate::kind`], except that
/// the five literals answer as one and a builtin call is named apart.
fn expr_kind(e: &Expr) -> &'static str {
    match e {
        Expr::Int(_, _)
        | Expr::Byte(_, _)
        | Expr::Float(_, _)
        | Expr::Bool(_, _)
        | Expr::Str(_, _) => "literal",
        Expr::Call { name, .. } if name.starts_with('@') => "builtin call",
        _ => crate::kind(e),
    }
}

fn gap<T>(what: &'static str, line: usize) -> Result<T, Gap> {
    Err(Gap {
        what,
        detail: String::new(),
        line,
        rule: None,
    })
}

fn gap_d<T>(what: &'static str, detail: &str, line: usize) -> Result<T, Gap> {
    Err(Gap {
        what,
        detail: detail.to_string(),
        line,
        rule: None,
    })
}

/// A rule the program breaks. Lowering stops as at a gap, and the placer
/// reports `rule` at `line`, with the ways out `more`, as it reports the
/// kernel's refusals.
fn refuse<T>(rule: Rule, more: Vec<String>, line: usize) -> Result<T, Gap> {
    Err(Gap {
        what: "a rule the program breaks",
        detail: String::new(),
        line,
        rule: Some(Box::new(crate::rules::refusal(line, rule, more))),
    })
}

/// Every builtin the row specifies, by name: each
/// [`vyrn_frontend::prelude::Builtin::spec`], then a [`Spec::Routes`] row per
/// route.
///
/// The emitter answers each from the row alone
/// (`vyrn_codegen::direct::Fn_::core_call`), so a name added here needs an
/// emission there. The codegen test `builtin_rows_all_emit` refuses a
/// [`Spec::Typed`] row with no instruction.
pub fn builtin_rows() -> &'static [(&'static str, Spec)] {
    static ROWS: std::sync::OnceLock<Vec<(&'static str, Spec)>> = std::sync::OnceLock::new();
    ROWS.get_or_init(|| {
        let all = prelude::builtins();
        let specs = all.iter().filter_map(|b| Some((b.name, b.spec.clone()?)));
        let routes = all.iter().flat_map(|b| {
            b.route
                .iter()
                .chain(&b.gen_route)
                .map(|f| (b.name, Spec::Routes(*f)))
        });
        specs.chain(routes).collect()
    })
}

/// The row [`builtin_rows`] holds for `name`. A routed name has a row per
/// route; a generator host's build takes [`vyrn_frontend::loader::routed_builtin`]'s.
pub fn builtin_row(name: &str, gen_host: bool) -> Option<&'static Spec> {
    let routed = vyrn_frontend::loader::routed_builtin(name, gen_host);
    builtin_rows()
        .iter()
        .find(|(n, s)| *n == name && routed.is_none_or(|f| matches!(s, Spec::Routes(g) if *g == f)))
        .map(|(_, s)| s)
}

/// The shapes among `body`'s rows that no emitter reads from the core, in
/// source order, each once; empty means the rows carry the body end to end.
/// Tags: `Call:<who>:<name>` for a callee the emitter's function table does
/// not answer, `Read:<kind>` and `Take:<kind>` for a place, `Opaque:<what>`
/// for a row that names no value, and `Lambda`. `tests/coredrive.rs` ranks
/// them.
pub fn gaps(body: &Body) -> Vec<String> {
    let mut out = Vec::new();
    for (s, _) in rows(&body.stmts) {
        if let St::Let(_, r) | St::Do { rhs: r, .. } = s {
            gaps_rhs(body, r, &mut out);
        }
        s.operands(&mut |v, _| {
            if let Val::Lit(Lit::Opaque(k)) = v {
                out.push(format!("Opaque:{k:?}"));
            }
        });
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|t| seen.insert(t.clone()));
    out
}

/// Ends every list of `ss` at its `trap`, because nothing after one runs.
/// An `if` or a `switch` whose every arm ends at one is a `trap` too.
///
/// Builders extend a list after a `panic` in value position (the join's
/// store, the call it feeds, an arm's releases), and a builder cannot cut its
/// own list because its caller extends it afterwards.
fn cut(ss: &mut Vec<St>) {
    ss.iter_mut().flat_map(St::lists_mut).for_each(cut);
    if let Some(i) = ss.iter().position(traps) {
        ss.truncate(i + 1);
    }
}

/// Whether every path through `s` ends at a `trap`. A `loop` or a `block`
/// may be left by a `break` before its last row, so neither is one.
fn traps(s: &St) -> bool {
    let ends = |ss: &[St]| ss.last().is_some_and(traps);
    match s {
        St::Trap => true,
        St::If { then, els, .. } => ends(then) && ends(els),
        St::Switch { arms, .. } => !arms.is_empty() && arms.iter().all(|a| ends(&a.body)),
        _ => false,
    }
}

/// Whether every path through `ss` ends in a `return` or a `trap`: the rule
/// "must return T on all paths", stated once for a function and a lambda.
/// A row after one that ends is never reached. A loop may run zero times or
/// be left by a `break`, so it ends nothing.
fn returns(ss: &[St]) -> bool {
    ss.iter().any(|s| match s {
        St::Return { .. } | St::Trap => true,
        St::If { then, els, .. } => returns(then) && returns(els),
        St::Switch { arms, .. } => !arms.is_empty() && arms.iter().all(|a| returns(&a.body)),
        St::Block { body, .. } => returns(body),
        St::Let(..)
        | St::Do { .. }
        | St::Store { .. }
        | St::Drop(..)
        | St::Row { .. }
        | St::Loop { .. }
        | St::Break { .. }
        | St::Continue { .. }
        | St::Check(_) => false,
    })
}

/// The refusal of a frame that can end without the value it owes, which
/// `rule` states from the type owed.
fn falls_through(body: &mut Body, owes: &Type, line: usize, rule: impl FnOnce(String) -> Rule) {
    if *owes != Type::Unit && !returns(&body.stmts) {
        body.refused.push((line, rule(owes.to_string()).render()));
    }
}

/// The tags of a right-hand side the emitter has no reader for.
fn gaps_rhs(body: &Body, r: &Rhs, out: &mut Vec<String>) {
    match r {
        // A take of a key has no reader; every other place read or take has.
        Rhs::Take(Place::Key(..)) => out.push("Take:Key".into()),
        Rhs::Call { callee, kind, .. } => {
            // The emitter reads a declared function, a constructor, a builtin
            // with a row, and a call through a stored value (one call to its
            // signature's dispatcher). A call through a `fn`-typed parameter
            // waits on the specialization. A routed name has a row under
            // either host, so the host is not read.
            if !matches!(
                kind,
                Callee::Fn(_) | Callee::Bound | Callee::Ctor | Callee::Named | Callee::Proven
            ) && builtin_row(callee, false).is_none()
                && !kind.value().is_some_and(|n| !body.params.contains(&n))
            {
                let tag = match kind {
                    Callee::Value(_) => "Value".to_string(),
                    k => format!("{k:?}"),
                };
                out.push(format!("Call:{tag}:{callee}"));
            }
        }
        Rhs::Prim(Op::Closure(_), ..) => out.push("Lambda".into()),
        // A part the emitter cannot place is the emitter's own screen, not a gap.
        Rhs::Val(_) | Rhs::Read(_) | Rhs::Take(_) | Rhs::Prim(..) | Rhs::Make(..) => {}
    }
}

/// Builds the core of one instance. The first build records candidates and
/// takes nothing; where [`last_owner`] names any, a second build takes them.
/// Its frames have no row ([`Body::id`]).
pub fn build(program: &Program, inst: &Instance<'_>, own: &Ownership) -> Result<Body, Gap> {
    build_in(program, inst, own, &Fns::default(), &mut Default::default())
}

/// [`build`] with the caller's function table and memo of `own`'s name facts.
pub(crate) fn build_in(
    program: &Program,
    inst: &Instance<'_>,
    own: &Ownership,
    fns: &Fns,
    names: &mut NameMemo,
) -> Result<Body, Gap> {
    build_from(program, own, fns, names, &Source::Instance(inst))
}

/// A projection of [`Lowered::places`] with its body, built.
pub(crate) type Projection<'l, 'a> = (&'l crate::PlaceBody<'a>, Result<Body, Gap>);

/// Builds each `impl` projection's body once, numbered in `fns`. A projection
/// is inlined at its site and no instance builds it, so the judgment reads
/// these bodies and [`augment`] types them.
pub(crate) fn build_places<'l, 'a>(
    program: &Program,
    lowered: &'l crate::Lowered<'a>,
    own: &Ownership,
    fns: &mut Fns,
) -> Vec<Projection<'l, 'a>> {
    let mut names = NameMemo::default();
    (lowered.places.iter())
        .map(|p| {
            let inst = Instance {
                func: p.func,
                func_id: p.id,
                type_args: Vec::new(),
                subst: Default::default(),
                facts: p.facts.clone(),
                releases: Vec::new(),
            };
            let mut body = build_in(program, &inst, own, fns, &mut names);
            if let Ok(b) = &mut body {
                fns.number(b);
            }
            (p, body)
        })
        .collect()
}

/// What a body is built from. The setup of each kind is [`seeded`]'s match.
enum Source<'s, 'a> {
    /// A function instance.
    Instance(&'s Instance<'a>),
    /// The body of a `test` or a `bench`: a block with no parameters, typed
    /// as a function returning Unit.
    Block(&'s OutsideBody<'a>),
    /// The module-state initializer: every module-scope `let` is a store into
    /// its global, run once at `_start` into an empty place. Its name is
    /// empty, the name the checker records a lambda written in it under
    /// (`StoredLambda::defined_in`), so a call through that lambda's type is
    /// judged over this frame.
    Globals(&'s NodeTypes<'a>),
    /// One module-state initializer or `where` predicate, for the typed
    /// judgment alone: it places no row and is never emitted. A predicate has
    /// `binds` (its `value` or its record's fields) and sees no module state.
    /// `facts` holds every root's; a refusal belongs to the root it is in.
    Expr {
        facts: &'s NodeTypes<'a>,
        file: Option<String>,
        binds: Option<&'s [(String, Type)]>,
        e: &'a Expr,
    },
}

impl Source<'_, '_> {
    /// Whether [`last_owner`] seeds a second build. A root takes nothing.
    fn two_pass(&self) -> bool {
        matches!(self, Source::Instance(_) | Source::Block(_))
    }
}

/// Builds the body of `source`. The first build records candidates and takes
/// nothing; where [`last_owner`] names any, a second build takes them.
fn build_from<'a>(
    program: &'a Program,
    own: &'a Ownership,
    fns: &'a Fns,
    names: &mut NameMemo,
    source: &Source<'_, 'a>,
) -> Result<Body, Gap> {
    let none = std::collections::HashSet::new();
    let b1 = vyrn_frontend::prof::phase("placer: build: first");
    let first = seeded(program, own, fns, names, source, &none)?;
    drop(b1);
    if !source.two_pass() {
        return Ok(first);
    }
    let seed = last_owner(&first);
    if seed.is_empty() {
        return Ok(first);
    }
    let _b2 = vyrn_frontend::prof::phase("placer: build: seeded");
    seeded(program, own, fns, names, source, &seed)
}

/// The refusals the typed judgment states over the checker's answers at the
/// expressions of `facts`: a literal that does not fit, a shift by a constant out of
/// range, and at a node the checker typed `Err` whose operands it typed, the
/// rule the node breaks (an operator, a field, a construction, a variant, a
/// record literal, or a call's arity, type arguments and arguments).
fn judged(facts: &NodeTypes<'_>, own: &Ownership, sp: &Speech) -> Vec<(usize, String)> {
    let decls = own.proto.types();
    let recorded = |e: &Expr| facts.types.get(&e.id()).filter(|t| **t != Type::Err);
    let resolved = |e: &Expr| recorded(e).map(|t| vyrn_frontend::types::resolve(t, decls));
    // Rules over a literal as written, whatever the checker typed it, so that
    // `vyrn check` refuses what the build would.
    let constant = |e: &Expr| -> Option<String> {
        use vyrn_frontend::checker::{int_literal_value, literal_value, misfit};
        let sized = |e: &Expr| match resolved(e)? {
            Type::IntN { bits, signed } => Some((bits, signed)),
            _ => None,
        };
        match e {
            Expr::Int(n, _) => {
                sized(e).and_then(|(b, s)| misfit("integer", literal_value(*n), b, s))
            }
            Expr::Unary {
                op: vyrn_frontend::ast::UnOp::Neg,
                expr,
                ..
            } if matches!(**expr, Expr::Int(_, _)) => {
                let v = int_literal_value(e)?;
                sized(e).and_then(|(b, s)| misfit("integer", v, b, s))
            }
            Expr::Byte(v, _) => sized(e).and_then(|(b, s)| misfit("byte", i128::from(*v), b, s)),
            Expr::Binary {
                op: BinOp::Match,
                rhs,
                ..
            } => Some(match &**rhs {
                Expr::Str(pat, _) => {
                    let err = vyrn_frontend::regex::compile(pat).err()?;
                    rule!(InvalidRegex, pat, err = err.to_string()).render()
                }
                _ => rule!(MatchNeedsPattern).render(),
            }),
            // A literal operand takes a sized sibling's type
            // (`Checker::expr`'s `adapt_int_literal`), the left one first.
            Expr::Binary { lhs, rhs, .. } => {
                [(lhs, rhs), (rhs, lhs)].into_iter().find_map(|(l, o)| {
                    let v = int_literal_value(l).filter(|_| resolved(l) == Some(Type::Int))?;
                    sized(o).and_then(|(b, s)| misfit("integer", v, b, s))
                })
            }
            Expr::ArrayLit { elems, .. } => {
                let limit = vyrn_frontend::trap::ARRAY_LIT_LIMIT;
                let len = elems.len();
                if len > limit {
                    return Some(rule!(ArrayLiteralTooLong, len, limit).render());
                }
                match resolved(e)? {
                    Type::SmallArray(_, n) if len > n => {
                        Some(rule!(SmallArrayOverflow, len, n).render())
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    };
    let refusal = |e: &Expr| -> Option<String> {
        if let Expr::Binary {
            op: BinOp::Shl | BinOp::Shr,
            rhs,
            ..
        } = e
        {
            let bits = match resolved(e)? {
                Type::IntN { bits, .. } => i64::from(bits),
                _ => 64,
            };
            let Some(vyrn_frontend::consteval::ConstVal::Int(amt)) =
                vyrn_frontend::consteval::eval(rhs, &HashMap::new())
            else {
                return None;
            };
            return (amt < 0 || amt >= bits).then(|| rule!(ShiftOutOfRange, amt, bits).render());
        }
        if let Some(s) = constant(e) {
            return Some(s);
        }
        if facts.types.get(&e.id()) != Some(&Type::Err) {
            return None;
        }
        match e {
            Expr::Unary { op, expr, .. } => {
                let ([t], []) = sp.say([&resolved(expr)?], []);
                let r = match op {
                    UnOp::Neg => rule!(NegNeedsNumber, t),
                    UnOp::Not => rule!(NotNeedsBool, t),
                    UnOp::BitNot => rule!(BitNotNeedsInteger, t),
                };
                Some(r.render())
            }
            Expr::Field { expr, field, .. } => Some(
                match resolved(expr)? {
                    Type::Record(_) => {
                        let ty = sp.ty(recorded(expr)?).to_string();
                        rule!(NoField, ty, field)
                    }
                    Type::Str if field == "length" => rule!(StringLength),
                    other => {
                        let ([other], []) = sp.say([&other], []);
                        rule!(FieldOnNonRecord, field, other)
                    }
                }
                .render(),
            ),
            Expr::TryConstruct { name, args, .. } | Expr::Call { name, args, .. }
                if decls.contains_key(name) =>
            {
                let base = &decls[name].base;
                let tries = matches!(e, Expr::TryConstruct { .. });
                let ([], [name]) = sp.say([], [name]);
                if tries && !matches!(base, Type::Int | Type::Bool | Type::Str) {
                    return Some(rule!(TryConstructNotScalar, name).render());
                }
                let [arg] = &args[..] else {
                    let got = args.len();
                    return Some(match tries {
                        true => rule!(TryConstructArity, name, got).render(),
                        false => rule!(ConstructArity, name, got).render(),
                    });
                };
                let ([base, aty], []) = sp.say([base, recorded(arg)?], []);
                Some(rule!(ConstructFrom, name, base, aty).render())
            }
            // Fields in written order, so the refusal names the first one the
            // reader sees; a missing field only once every value typed.
            Expr::StructLit { name, fields, .. } => {
                if !decls.contains_key(name) {
                    return Some(rule!(UnknownType, n = name).render());
                }
                let named = Type::Named(name.clone());
                let declared = vyrn_frontend::types::record_fields(&named, decls);
                let ([], [name]) = sp.say([], [name]);
                let Some(declared) = declared else {
                    return Some(rule!(NotRecordType, name).render());
                };
                for (k, (field, _)) in fields.iter().enumerate() {
                    if !declared.iter().any(|f| &f.name == field) {
                        return Some(rule!(RecordNoField, name, field).render());
                    }
                    if fields[..k].iter().any(|(g, _)| g == field) {
                        return Some(rule!(FieldSetTwice, field).render());
                    }
                }
                fields
                    .iter()
                    .try_for_each(|(_, v)| recorded(v).map(|_| ()))?;
                let f = declared
                    .iter()
                    .find(|f| fields.iter().all(|(g, _)| *g != f.name))?;
                Some(rule!(MissingField, field = f.name, name).render())
            }
            Expr::Var { name, .. } => {
                let payload = decls
                    .values()
                    .filter_map(|d| vyrn_frontend::types::declared_variants(&d.base))
                    .flatten()
                    .find(|v| &v.name == name)?
                    .payload
                    .len();
                Some(rule!(VariantNeedsArgs, name, payload).render())
            }
            Expr::Call {
                args,
                type_args,
                dot,
                ..
            } => {
                let d = call_decl(own, e.id())?;
                let ([], [shown]) = sp.say([], [&d.shown]);
                // Counts are of what the reader wrote after the dot (#577). A
                // callee with no parameters has no receiver slot.
                let dot = usize::from(*dot && !d.params.is_empty());
                if d.params.len() != args.len() {
                    let (want, got) = (d.params.len() - dot, args.len() - dot);
                    return Some(rule!(CallArity, shown, want, got).render());
                }
                // A type argument to a callee that declares none would look
                // honoured if accepted. A method call reads none.
                if !d.recv && !type_args.is_empty() && d.type_params == 0 {
                    return Some(rule!(NoTypeParams, shown).render());
                }
                if !d.recv && type_args.len() > d.type_params {
                    let (want, got) = (d.type_params, type_args.len());
                    return Some(rule!(TypeArity, name = shown, want, got).render());
                }
                // The first argument its parameter does not take. A `fn`
                // parameter's is the checker's `check_fn_arg`.
                args.iter()
                    .zip(&d.params)
                    .enumerate()
                    .skip(usize::from(d.recv))
                    .find_map(|(i, (a, pty))| {
                        let aty = recorded(a)?;
                        let fits = matches!(pty, Type::Fn(..))
                            || vyrn_frontend::types::coercible(aty, pty, decls);
                        (!fits).then(|| {
                            let ([pty, aty], [shown]) = sp.say([pty, aty], [&d.shown]);
                            match i.checked_sub(dot) {
                                Some(k) => {
                                    rule!(FnValueArgType, name = shown, arg = k + 1, pty, aty)
                                }
                                None => rule!(ReceiverType, shown, pty, aty),
                            }
                            .render()
                        })
                    })
            }
            _ => None,
        }
    };
    facts
        .exprs
        .iter()
        .filter_map(|(e, line)| Some((*line as usize, refusal(e)?)))
        .collect()
}

/// The calls in `facts` whose solved type argument fails a bound of the
/// callee's seeded row: `@Heapless` (`clear`, `append`, `copyFrom`),
/// `@Decodable` (`fromJson`) and `Show` (`print`, `toString`). A type
/// parameter of the body satisfies `Show` where `outer`, the body's bounds,
/// gives it. A type argument that fails as written (`Array<T>`) is named as
/// written, so every instance gives one sentence.
fn unbound(
    facts: &NodeTypes<'_>,
    own: &Ownership,
    impls: &vyrn_frontend::types::Impls,
    outer: &HashMap<String, Vec<String>>,
    sp: &Speech,
) -> Vec<(usize, String)> {
    use vyrn_frontend::prelude::{signature, DECODABLE, HEAPLESS};
    use vyrn_frontend::types::{self, SHOW};
    let decls = own.proto.types();
    let fails = |t: &Type, bound: &str| {
        let base = types::resolved(t, decls);
        match bound {
            HEAPLESS => vyrn_frontend::declared::owns_heap(&base, decls),
            DECODABLE => vyrn_frontend::codec::decodable(&base, decls).is_err(),
            SHOW => match &*base {
                Type::Param(p) => !outer.get(p).is_some_and(|bs| bs.iter().any(|b| b == SHOW)),
                _ => !types::renders(&base) && types::show_dispatch(impls, t, &base).is_none(),
            },
            _ => false,
        }
    };
    let sentence = |shown: &str, t: &Type, bound: &str| match bound {
        HEAPLESS => {
            let ([t], []) = sp.say([t], []);
            rule!(ForgetsHeap, shown, t).render()
        }
        DECODABLE => {
            // The codec names the offender by its declaration where it has one.
            let off = match vyrn_frontend::codec::decodable(t, decls) {
                Err(off) => sp.name(&off),
                Ok(()) => sp.ty(t).to_string(),
            };
            rule!(NotCodable, shown, off).render()
        }
        _ => types::needs_show(shown, t).render_in(sp),
    };
    facts
        .exprs
        .iter()
        .filter_map(|(e, _)| {
            let Expr::Call { name, .. } = e else {
                return None;
            };
            let bounds = &signature(name)?.type_bounds;
            let written = node_solved(own, e.id()).unwrap_or_default();
            let shown = prelude::method_surface(name).trim_start_matches('@');
            let solved = facts.solved.get(&e.id())?;
            solved.iter().find_map(|(tp, t)| {
                let w = written.iter().find(|(p, _)| p == tp).map_or(t, |(_, w)| w);
                if [t, w].iter().any(|t| types::resolve(t, decls) == Type::Err) {
                    return None;
                }
                bounds.get(tp)?.iter().find_map(|b| {
                    let named = [w, t].into_iter().find(|t| fails(t, b))?;
                    Some((e.line(), sentence(shown, named, b)))
                })
            })
        })
        .collect()
}

/// How a function value misses the `fn` type of its slot, a call's parameter
/// or a stored slot; each words it for its slot.
enum Misfit {
    /// The value takes this many parameters, and the slot passes that many.
    Arity(usize, usize),
    /// The value's parameter, and what the slot passes it.
    Param(Type, Type),
    /// What the value returns, and what the slot returns.
    Returns(Type, Type),
}

/// The first way a value misses `slot`: its arity, then each of `params`
/// (`None` where the slot types them, as it does a lambda's), then `ret`
/// (`None` where the slot does not ask). A slot that returns `Unit` or a type
/// parameter takes any return.
fn misfit(
    arity: usize,
    params: Option<&[Type]>,
    ret: Option<&Type>,
    slot: &Type,
    decls: &HashMap<String, TypeDecl>,
) -> Option<Misfit> {
    use vyrn_frontend::types::{assignable, coercible, resolve};
    let Type::Fn(ptys, sret) = resolve(slot, decls) else {
        return None;
    };
    if arity != ptys.len() {
        return Some(Misfit::Arity(arity, ptys.len()));
    }
    let mut both = params.into_iter().flatten().zip(&ptys);
    if let Some((a, b)) = both.find(|(a, b)| !assignable(b, a, decls)) {
        return Some(Misfit::Param(a.clone(), b.clone()));
    }
    let got = ret.filter(|_| !matches!(*sret, Type::Unit | Type::Param(_)))?;
    (!coercible(got, &sret, decls)).then(|| Misfit::Returns(got.clone(), *sret))
}

/// The refusal of a call to `name` whose `fn` argument does not fit its
/// parameter. `solved` is the instance as far as the checker solved it; for
/// a body as written it names its own type parameters. `bound` answers
/// whether a name is a binding, which a function of that name does not
/// shadow.
#[allow(clippy::too_many_arguments)]
fn fn_slot(
    program: &Program,
    name: &str,
    args: &[Expr],
    line: usize,
    solved: &[(String, Type)],
    types: &HashMap<NodeId, Type>,
    decls: &HashMap<String, TypeDecl>,
    bound: &dyn Fn(&str) -> bool,
    sp: &Speech,
) -> Option<(usize, String)> {
    use vyrn_frontend::types::{resolve, substitute};
    let recorded = |e: &Expr| types.get(&e.id()).filter(|t| **t != Type::Err);
    let f = program.functions.iter().find(|f| f.name == name)?;
    let ([], [callee]) = sp.say([], [prelude::method_surface(name).trim_start_matches('@')]);
    let subst: HashMap<String, Type> = solved.iter().cloned().collect();
    let mut slots = f.params.iter().zip(args).enumerate();
    slots.find_map(|(i, (p, arg))| {
        let slot = substitute(&p.ty, &subst);
        let Type::Fn(ptys, _) = &slot else {
            return None;
        };
        let n = i + 1;
        let want = ptys.len();
        // A value of `fn` type, named `subject` in the arity sentence and
        // `owner` in the parameter one.
        let value = |subject: &str, owner: &str, vptys: &[Type]| {
            let r = match misfit(vptys.len(), Some(vptys), None, &slot, decls)? {
                Misfit::Arity(got, _) => rule!(ValueArity, subject, got, callee, n, want),
                Misfit::Param(a, b) => {
                    let ([a, b], []) = sp.say([&a, &b], []);
                    rule!(ValueParam, owner, a, callee, b)
                }
                Misfit::Returns(..) => return None,
            };
            Some(r.render())
        };
        let says = match arg {
            Expr::Lambda {
                params,
                body,
                line: at,
                ..
            } => {
                let got = match body {
                    LambdaBody::Expr(e) => recorded(e),
                    LambdaBody::Block(_) => None,
                };
                let says = match misfit(params.len(), None, got, &slot, decls)? {
                    Misfit::Arity(got, _) => rule!(LambdaArgArity, got, callee, n, want),
                    Misfit::Returns(t, r) => {
                        let ([t, r], []) = sp.say([&t, &r], []);
                        rule!(LambdaReturns, t, callee, r)
                    }
                    Misfit::Param(..) => return None,
                };
                return Some((*at, says.render()));
            }
            Expr::Var { name: vn, .. } if bound(vn) => match recorded(arg)
                .map(|t| resolve(t, decls))
            {
                Some(Type::Fn(vptys, _)) => value(&format!("`{vn}`"), &format!("`{vn}`"), &vptys),
                _ => None,
            },
            Expr::Var { name: vn, .. } => {
                let g = program.functions.iter().find(|g| g.name == *vn)?;
                if !g.type_params.is_empty() {
                    return Some((line, rule!(GenericFnArg, vn).render()));
                }
                let vptys: Vec<Type> = g.params.iter().map(|p| p.ty.clone()).collect();
                let ([], [vn]) = sp.say([], [vn]);
                match misfit(vptys.len(), Some(&vptys), None, &slot, decls)? {
                    Misfit::Arity(got, _) => {
                        Some(rule!(FnArity, vn, got, callee, n, want).render())
                    }
                    Misfit::Param(a, b) => {
                        let ([a, b], []) = sp.say([&a, &b], []);
                        let owner = format!("`{vn}`");
                        Some(rule!(ValueParam, owner, a, callee, b).render())
                    }
                    Misfit::Returns(..) => None,
                }
            }
            other => {
                let aty = recorded(other)?;
                match resolve(aty, decls) {
                    Type::Fn(vptys, _) => value("this", "this function value", &vptys),
                    _ => {
                        let at = match other.line() {
                            0 => line,
                            l => l,
                        };
                        let ([aty], []) = sp.say([aty], []);
                        return Some((at, rule!(NotFnArg, callee, n, aty).render()));
                    }
                }
            }
        };
        says.map(|says| (line, says))
    })
}

/// The refusal of a value stored in a slot of `fn` type `exp` that it does
/// not fit: a lambda whose body returns `lambda`, or the function `f`. The
/// checker refuses a lambda's parameter count itself.
fn stored_slot(
    lambda: Option<&Type>,
    f: Option<&Function>,
    exp: &Type,
    decls: &HashMap<String, TypeDecl>,
    line: usize,
    sp: &Speech,
) -> Option<(usize, String)> {
    let says = match (lambda, f) {
        (Some(got), _) => {
            let Type::Fn(ptys, _) = vyrn_frontend::types::resolve(exp, decls) else {
                return None;
            };
            match misfit(ptys.len(), None, Some(got), exp, decls)? {
                Misfit::Returns(t, r) => {
                    let ([t, exp, r], []) = sp.say([&t, exp, &r], []);
                    rule!(LambdaReturnsSlot, t, exp, r)
                }
                Misfit::Arity(..) | Misfit::Param(..) => return None,
            }
        }
        (None, Some(f)) => {
            let vptys: Vec<Type> = f.params.iter().map(|p| p.ty.clone()).collect();
            match misfit(vptys.len(), Some(&vptys), Some(&f.ret), exp, decls)? {
                Misfit::Arity(got, want) => {
                    let ([exp], [name]) = sp.say([exp], [&f.name]);
                    rule!(FnAritySlot, name, got, exp, want)
                }
                Misfit::Param(a, b) => {
                    let ([exp, a, b], [name]) = sp.say([exp, &a, &b], [&f.name]);
                    let owner = format!("`{name}`");
                    rule!(ValueParam, owner, a, callee = exp, b)
                }
                Misfit::Returns(t, r) => {
                    let ([exp, t, r], [name]) = sp.say([exp, &t, &r], [&f.name]);
                    rule!(FnReturnsSlot, name, t, exp, r)
                }
            }
        }
        (None, None) => return None,
    };
    Some((line, says.render()))
}

fn seeded<'a>(
    program: &'a Program,
    own: &'a Ownership,
    fns: &'a Fns,
    names: &mut NameMemo,
    source: &Source<'_, 'a>,
    seed: &std::collections::HashSet<NodeId>,
) -> Result<Body, Gap> {
    let mut b = Builder::new(program, own, fns, names, source, seed);
    let mut out = Vec::new();
    match source {
        Source::Instance(inst) => {
            let f: &Function = inst.func;
            // A parameter's type is the instance's, not the declaration's:
            // `map<Int64, Int64>`'s `f` is `fn(Int64) -> Int64`, the shape
            // stored sources are keyed by. Every other type comes substituted
            // in the rows.
            let subst: HashMap<String, Type> = inst.subst.clone().into_iter().collect();
            b.frame.ret = Some(vyrn_frontend::types::substitute(&f.ret, &subst));
            // A declared release (`impl Owned for T { fn release(consume self) }`)
            // frees `self`'s parts, and nothing releases `self` again: the
            // kernel owns `self` there, so a part taken twice is refused, but
            // owes no release of it.
            let is_release = b.proto.is_release_fn(&f.name);
            for p in &f.params {
                let pty = vyrn_frontend::types::substitute(&p.ty, &subst);
                let owned = p.capability == Capability::Consume && b.owns(&pty) && !is_release;
                let n = b.name(&p.name, pty, owned, f.line);
                if is_release {
                    b.frame.released = Some(n);
                    b.body.names[n.index()].borrow = false;
                }
                // A `read` or `modify` parameter is never taken; the
                // kernel needs the capability to word the refusal. A must-use
                // parameter is excepted from the take only: its capability
                // still words a refusal about a second name for it.
                b.body.names[n.index()].must_use_param =
                    b.proto.must_use(&b.body.names[n.index()].ty.clone());
                b.body.names[n.index()].borrow_kind = param_borrow(p.capability, &p.name, false);
                b.body.names[n.index()].mutable = p.capability == Capability::Modify;
                b.frame.scope.push((p.name.clone(), n));
                b.keyed(n, p.id());
                b.body.params.push(n);
            }
            // Every call row checks the clauses, so every entry has them. A
            // clause the checker refused states nothing.
            let params = &b.body.params;
            let param = |k: usize| params.get(k).map(|n| Val::Name(*n));
            let atoms = vyrn_frontend::core::check::clauses(f, b.proto.types()).unwrap_or_default();
            b.body.assumes = (atoms.iter())
                .flat_map(|(_, atoms)| atoms)
                .filter_map(|a| a.with(&param))
                .collect();
            b.frame.appends = crate::append::append_candidates(&f.body);
            rebound(&f.body, &mut b.frame.rebound);
            b.block(&f.body, &mut out)?;
            cut(&mut out);
            b.body.stmts = out;
            let name = b.body.spelled(&f.name).to_string();
            falls_through(&mut b.body, &f.ret, f.line, |owes| {
                rule!(FunctionFallsThrough, name, owes)
            });
        }
        Source::Block(ob) => {
            b.frame.ret = Some(Type::Unit);
            b.frame.appends = crate::append::append_candidates(ob.block);
            rebound(ob.block, &mut b.frame.rebound);
            b.block(ob.block, &mut out)?;
            cut(&mut out);
            b.body.stmts = out;
        }
        Source::Globals(_) => {
            for g in &program.globals {
                // A crossing into a validated declared type is its constructor.
                let check = match &g.ty {
                    Some(to) => b.checked(&b.ty_of(&g.init)?, to, &g.init),
                    None => None,
                };
                let v = match check {
                    Some(to) => Val::Name(b.checked_temp(&to, &g.init, g.line, &mut out)?),
                    None => b.val(&g.init, &mut out)?,
                };
                out.push(St::Store {
                    place: Place::Global(g.name.clone()),
                    value: v,
                    old: Old::Nothing,
                    line: g.line,
                    site: Site::None,
                    releases: false,
                    holes: Vec::new(),
                });
            }
            cut(&mut out);
            b.body.stmts = out;
        }
        Source::Expr { binds, e, .. } => {
            b.closed = binds.is_some();
            for (name, ty) in binds.unwrap_or_default() {
                let n = b.name(name, ty.clone(), false, e.line());
                b.frame.scope.push((name.clone(), n));
                b.body.params.push(n);
            }
            // A gap under a refusal the builder states is that refusal, as in
            // [`Builder::stmt_list`].
            if let Err(g) = b.val(e, &mut out) {
                if b.body.refused.is_empty() && b.body.mistyped.is_empty() {
                    return Err(g);
                }
            }
            b.body.stmts = out;
        }
    }
    Ok(b.body)
}

/// The module-state initializers as a body ([`Source::Globals`]).
pub fn build_module_state<'a>(
    program: &'a Program,
    own: &'a Ownership,
    fns: &'a Fns,
    facts: &NodeTypes<'a>,
) -> Result<Body, Gap> {
    build_from(
        program,
        own,
        fns,
        &mut NameMemo::default(),
        &Source::Globals(facts),
    )
}

/// The body of a `test` or a `bench` ([`Source::Block`]).
pub fn build_outside<'a>(
    program: &'a Program,
    own: &'a Ownership,
    fns: &'a Fns,
    names: &mut NameMemo,
    ob: &OutsideBody<'a>,
) -> Result<Body, Gap> {
    build_from(program, own, fns, names, &Source::Block(ob))
}

/// A `for` whose every element leaves through the loop variable
/// ([`Body::loop_buffers`]): the container, its length, and the counter,
/// which steps past an element as the turn binds it.
#[derive(Clone)]
struct Unreached {
    it: Name,
    n: Name,
    i: Name,
    elem: Type,
    line: usize,
}

struct Builder<'a> {
    program: &'a Program,
    own: &'a Ownership,
    /// The function table a frame reads its row from ([`Body::id`]). A
    /// worker only reads it.
    fns: &'a Fns,
    proto: &'a Owned,
    names: &'a mut NameMemo,
    /// The bounds of the function's type parameters; `None` for a body that
    /// is no function.
    bounds: Option<&'a HashMap<String, Vec<String>>>,
    types: HashMap<NodeId, Type>,
    /// The producer type of every typed expression, before the destination's
    /// coercion (see [`Rhs`]); `types` holds what the value must end up as.
    produced: HashMap<NodeId, Type>,
    solved: HashMap<NodeId, Vec<(String, Type)>>,
    placed: HashMap<(Exit, NodeId), Vec<&'a Release>>,
    body: Body,
    frame: Frame,
    temps: u32,
    /// The constructs this build may take their named scrutinee at, as
    /// [`last_owner`] decided over the previous build. Empty on the first.
    seed: &'a std::collections::HashSet<NodeId>,
    /// A `where` predicate's body: it sees its binds and no module state.
    closed: bool,
}

/// The state of the body a [`Builder`] is building: its scopes, loops,
/// pending temporaries and names. A lambda's body is built in a frame of its
/// own, which [`Builder::lambda_frame`] swaps in whole and swaps back.
#[derive(Default)]
struct Frame {
    /// The body's String accumulators ([`crate::append::append_candidates`]).
    appends: std::collections::HashSet<String>,
    /// The names the body stores into whole ([`rebound`]).
    rebound: std::collections::HashSet<String>,
    scope: Vec<(String, Name)>,
    /// The statement being built ([`NameInfo::stmt`]).
    stmt: NodeId,
    /// The plan keys a release by the node that owns the value: a `Stmt::Let`,
    /// a parameter, or the construct that owns a temporary.
    by_binding: HashMap<NodeId, Name>,
    /// An unnamed receiver minted for a field or element read, with the node
    /// that produced it, so the read can release it when the plan says the
    /// frame owns it.
    pending_receiver: Option<(Name, NodeId, bool)>,
    /// How many non-lending calls and operators enclose the expression being
    /// built. The compiled backends drain argument temporaries at each, so a
    /// receiver borrowed under one can be freed there.
    drain: u32,
    /// The scrutinee expression being built, which is read and not taken.
    scrutinee: Option<NodeId>,
    /// Temporaries the expression being built has read and must release once
    /// it is bound (`read_val`, `call`, `rhs`, `bind`).
    after: Vec<Name>,
    /// What `rhs` left for the binding that follows it.
    after_of_rhs: Vec<Name>,
    /// The owning temporaries evaluated for a consumer that has not run yet,
    /// which a `?` in a later operand releases on its failure exit
    /// ([`Builder::leave_try`]). An `rhs` and a join arm truncate it to its
    /// length at entry; a statement and a lambda frame start it empty.
    held: Vec<Name>,
    /// The check `rhs` owes a record literal of a validated type, with its
    /// line: [`Builder::bind`] states it after the literal's row.
    owed: Option<(String, usize)>,
    /// The streams the enclosing `for` loops walk, innermost last. A `return`
    /// or a `?` inside closes all of them (the direct backend's cursor
    /// stack); the loop's end closes its own.
    stream_loops: Vec<Name>,
    /// One entry per enclosing loop, innermost last: the `for` whose elements
    /// no turn reached yet, or `None`. A `return`, a `?` and a `break` release
    /// them ([`Builder::release_unreached`]).
    walks: Vec<Option<Unreached>>,
    /// One entry per enclosing loop: the name count when its body opened. A
    /// name below the innermost entry is bound outside the loop, so handing
    /// it out of a join arm frees it once per turn ([`Builder::alias_out`]).
    loop_marks: Vec<usize>,
    /// A join's result, and the name an arm handed out of it from outside the
    /// enclosing loop. [`Builder::loop_alias`] refuses where something owns
    /// the result and releases it once per turn.
    loop_aliased: HashMap<Name, (String, Option<(usize, usize)>)>,
    /// Whether the value being lowered is a rebind's. The slot is released by
    /// its final value, so the temporary the value passes through owns
    /// nothing and the back edge repeats no release (`std/html.vyrn`'s
    /// `attrKey`).
    rebinding: bool,
    /// Whether the argument position being lowered may keep what it is
    /// handed: `Some(false)` where it provably only borrows, `Some(true)`
    /// where it may store it, `None` outside an argument. Only the lambda
    /// arms read it.
    call_keeps: Option<bool>,
    /// [`NameInfo::closure_reads`] for the lambda [`Builder::rhs`] has just
    /// built, waiting for the name [`Builder::bind`] gives it.
    pending_closure: Option<Vec<Name>>,
    /// Whether the row [`Builder::rhs`] has just built is a copy the reader
    /// did not write ([`Builder::copy_rhs`]), waiting for the name
    /// [`Builder::bind`] gives it.
    pending_copy: bool,
    /// The receivers of the projections being inlined, innermost last. A
    /// projection declares `read self`, so no construct of its body is its
    /// receiver's last owner ([`Builder::takes_scrutinee`]).
    reading: Vec<Name>,
    /// How many `region`s enclose the statement. An arena buffer cannot
    /// grow, so an append inside one is the `concat` call.
    region: u32,
    /// The frame's declared result, which a `?` on an `Option` fails with
    /// `None` of. `None` for module state, an outside block and an untyped
    /// lambda.
    ret: Option<Type>,
    /// The receiver of a declared release, which the frame does not own
    /// ([`Builder::owns_boxes`]).
    released: Option<Name>,
}

impl<'a> Builder<'a> {
    /// A builder for the body of `source`. The row is an instance's own
    /// ([`Fns::instance_id`]), not the first row under its name: a projection
    /// can share a function's name. The type parameters' bounds in scope are
    /// an instance's, `None` for a body that is no instance.
    fn new(
        program: &'a Program,
        own: &'a Ownership,
        fns: &'a Fns,
        names: &'a mut NameMemo,
        source: &Source<'_, 'a>,
        seed: &'a std::collections::HashSet<NodeId>,
    ) -> Self {
        let filtered;
        let (facts, file) = match source {
            Source::Instance(inst) => (&inst.facts, inst.func.module.clone()),
            Source::Block(ob) => (&ob.facts, ob.module.clone()),
            Source::Globals(facts) => (*facts, None),
            Source::Expr { facts, file, e, .. } => {
                let mut mine = Vec::new();
                vyrn_frontend::ast::node_ids(e, &mut mine);
                let mine: std::collections::HashSet<NodeId> = mine.into_iter().collect();
                filtered = NodeTypes {
                    exprs: facts
                        .exprs
                        .iter()
                        .filter(|(x, _)| mine.contains(&x.id()))
                        .copied()
                        .collect(),
                    ..(*facts).clone()
                };
                (&filtered, file.clone())
            }
        };
        let (rows, bounds, id, name, export) = match source {
            Source::Instance(inst) => (
                Some(inst.func_id),
                Some(&inst.func.type_bounds),
                fns.instance_id(inst),
                inst.spelling(),
                inst.func.is_export_extern,
            ),
            Source::Block(ob) => (Some(ob.id), None, fns.id(&ob.name), ob.name.clone(), false),
            Source::Globals(_) | Source::Expr { .. } => {
                (None, None, fns.id(""), String::new(), false)
            }
        };
        // The plan's own rows, not the instance's copy: the copy predates the
        // rows [`augment`] places. The copy adds only the substituted type a
        // `Deep` walks, and nothing below reads a kind.
        let mut placed: HashMap<(Exit, NodeId), Vec<&Release>> = HashMap::new();
        for r in rows
            .and_then(|id| own.releases.get(&id))
            .into_iter()
            .flatten()
        {
            placed.entry((r.exit, r.site)).or_default().push(r);
        }
        let sp = program.spellings.speech(&file);
        let none = HashMap::new();
        let (refused, mistyped) = (
            unbound(facts, own, &program.impls, bounds.unwrap_or(&none), &sp),
            judged(facts, own, &sp),
        );
        Builder {
            program,
            own,
            fns,
            proto: &own.proto,
            names,
            bounds,
            types: facts.types.clone(),
            produced: facts.produced.clone(),
            solved: facts.solved.clone(),
            placed,
            body: Body {
                id,
                name,
                spellings: program.spellings.clone(),
                file,
                export,
                names: Vec::new(),
                params: Vec::new(),
                assumes: Vec::new(),
                stmts: Vec::new(),
                lambdas: Vec::new(),
                cands: Vec::new(),
                loop_buffers: Vec::new(),
                unbound_drops: Vec::new(),
                refused,
                mistyped,
                ends: HashMap::new(),
                consumes: HashMap::new(),
            },
            frame: Frame::default(),
            temps: 0,
            seed,
            closed: false,
        }
    }

    fn owns(&self, ty: &Type) -> bool {
        self.proto.owns_heap(ty) || self.proto.must_use(ty) || self.proto.release_kind(ty).is_some()
    }

    /// Whether the value a name holds is the name's to release at a store: a
    /// name that releases it at the end of the frame, or a `modify` parameter,
    /// whose slot the caller keeps but whose old value the store replaces.
    fn slot_owns(&self, n: Name) -> bool {
        let info = &self.body.names[n.index()];
        info.releases || (info.is_modify_param() && self.owns(&info.ty))
    }

    fn name(&mut self, source: &str, ty: Type, releases: bool, line: usize) -> Name {
        let (heap, linear, runs) = self.proto.name_facts(&ty, self.names);
        self.body.names.push(NameInfo {
            source: source.to_string(),
            ty,
            releases,
            heap,
            borrow: heap && !releases,
            borrow_kind: None,
            line,
            stmt: self.frame.stmt,
            binding: None,
            receiver: None,
            producer: None,
            arg_drop: None,
            holes: Vec::new(),
            payload: None,
            receiver_malloc: false,
            grows: false,
            must_use_param: false,
            path: None,
            for_consume: false,
            fields: Vec::new(),
            loop_var: None,
            walked: None,
            linear,
            bound_by_let: false,
            implicit_copy: false,
            mutable: false,
            closure_reads: None,
            not_owned: None,
            runs,
        });
        Name((self.body.names.len() - 1) as u32)
    }

    /// The path a reader wrote for a place read, as the checker quotes it
    /// (`p.name`, `xs[i]`), with a renamed global as its module wrote it.
    /// `None` where the expression names no place.
    fn reader_path(&self, e: &Expr) -> Option<String> {
        let (root, path) = vyrn_frontend::ast::place_path(e)
            .or_else(|| vyrn_frontend::project::element_path(e, &self.own.place_names))?;
        // `path` starts with `root`.
        Some(format!("{}{}", self.written(&root), &path[root.len()..]))
    }

    /// The variable `name` as the reader wrote it: a binding as itself, module
    /// state as its module wrote it.
    fn written<'n>(&'n self, name: &'n str) -> &'n str {
        match self.lookup(name) {
            Some(_) => name,
            None => self.body.spelled(name),
        }
    }

    /// The `@borrow` a read of a place binds, carrying the path the reader
    /// wrote ([`NameInfo::path`]).
    fn borrow_name(&mut self, e: &'a Expr, ty: Type, line: usize) -> Name {
        let n = self.name("@borrow", ty, false, line);
        self.body.names[n.index()].path = self.reader_path(e);
        self.spell_take(e, n);
        n
    }

    /// Records where `e`, taken as `n`, ends in the reader's text
    /// ([`Body::ends`]): a name, a field or an element, in the root module's own source.
    fn spell_take(&mut self, e: &Expr, n: Name) {
        if self.frame.stmt == NodeId::NONE || !self.body.names[n.index()].heap {
            return;
        }
        if let Some(at) = self.spelled_end(e) {
            self.body.ends.entry((self.frame.stmt, n)).or_insert(at);
        }
    }

    /// The line and column just past `e`, a name, a field or an element,
    /// where the reader spelled it in the root module's own source.
    fn spelled_end(&self, e: &Expr) -> Option<(usize, usize)> {
        let (end, root) = match e {
            Expr::Var { name, id, .. } => (id.col() + name.chars().count(), id),
            Expr::Field { field, id, .. } => (id.col() + field.chars().count(), id),
            // An element is spelled at its `]`.
            Expr::Call { name, id, .. } if name == "@at" => (id.col() + 1, id),
            _ => return None,
        };
        let spelled =
            root.col() > 0 && root.0.unit() < NodeId::EXPANDED && self.body.file.is_none();
        spelled.then(|| (e.line(), end))
    }

    /// The edits that replace `consume PLACE` by `PLACE.copy()` where the
    /// reader wrote it on one line of the root module's own source: delete the
    /// keyword at column `kw` of `kw_line`, copy past the place. Empty where the text does
    /// not place the take.
    fn uncopy(&self, place: &Expr, (kw_line, kw): (usize, usize)) -> Vec<Fix> {
        let mut root = place;
        while let Expr::Field { expr, .. } = root {
            root = expr;
        }
        match (self.spelled_end(place), root) {
            (Some((line, end)), Expr::Var { line: l, id, .. })
                if kw > 0 && *l == line && line == kw_line =>
            {
                vec![
                    Fix::Unconsume {
                        line,
                        col: kw,
                        len: id.col().saturating_sub(kw),
                    },
                    Fix::Copy { line, col: end },
                ]
            }
            _ => Vec::new(),
        }
    }

    /// [`Builder::keyed`] for a `let` the reader wrote.
    fn keyed_let(&mut self, n: Name, s: &Stmt) {
        self.keyed(n, s.id());
        self.body.names[n.index()].bound_by_let = true;
        self.body.names[n.index()].mutable = matches!(s, Stmt::Let { mutable: true, .. });
    }

    /// Records the plan's key for a name, and the name for the key.
    fn keyed(&mut self, n: Name, binding: NodeId) {
        assert_ne!(binding, NodeId::NONE, "a binding keyed by no node");
        self.body.names[n.index()].binding = Some(binding);
        self.frame.by_binding.insert(binding, n);
    }

    /// Refuses binding, by a `let` or an argument temporary, a join whose arm
    /// handed out a name bound outside the enclosing loop: the binding
    /// releases it every turn ([`Builder::alias_out`]).
    fn loop_alias(&self, rhs: &Rhs, line: usize) -> Result<(), Gap> {
        let Rhs::Val(Val::Name(m)) = rhs else {
            return Ok(());
        };
        let Some((a, end)) = self.frame.loop_aliased.get(m) else {
            return Ok(());
        };
        let fixes = (end.map(|(line, col)| Fix::Copy { line, col }))
            .into_iter()
            .collect();
        refuse(rule!(HandedOutOfLoopArm, a), Vec::new(), line).map_err(|g| Gap {
            rule: g.rule.map(|d| Box::new(d.with_fixes(fixes))),
            ..g
        })
    }

    /// Whether a `let` binds a value this frame owns, read off the lowered
    /// `Rhs`. Where the type owns heap or carries an obligation, the frame
    /// still does not own:
    ///
    ///   - a static value (a literal, a nullary constructor) in an immutable
    ///     binding; a `mut` slot is released by its final value, so
    ///     `let mut acc: String = ""` owns;
    ///   - a read of a place, or a second name for a borrow.
    ///
    /// A rebind states the same rule at the store (`Stmt::Assign`).
    fn owned_binding(&self, rhs: &Rhs, ty: &Type, static_value: bool, mutable: bool) -> bool {
        if !self.owns(ty) {
            return false;
        }
        if static_value && !mutable {
            return false;
        }
        match rhs {
            Rhs::Read(_) => false,
            Rhs::Val(Val::Name(m)) => !self.body.names[m.index()].borrow,
            _ => true,
        }
    }

    /// Whether the `let` `s` of type `ty` is a copy (#501): a `let mut` the
    /// body stores into whole, bound to a borrow of a type that owns heap.
    /// The binding owns its value, so its stores and exit release on every
    /// path, where a borrow would release the owner's value or nothing. A
    /// type with `impl Copy` keeps the borrow.
    fn copies(&self, s: &Stmt, ty: &Type) -> bool {
        let Stmt::Let {
            name,
            value,
            mutable,
            ..
        } = s
        else {
            return false;
        };
        let borrow = match value {
            Expr::Var { name: m, .. } => self
                .lookup(m)
                .is_some_and(|m| self.body.names[m.index()].borrow),
            e => is_place_read(e) && !self.forces(e),
        };
        *mutable
            && borrow
            && self.frame.rebound.contains(name)
            && self.owns(ty)
            && (self.program.impls)
                .method(
                    vyrn_frontend::types::COPY,
                    ty,
                    vyrn_frontend::types::COPY_COPY,
                )
                .is_none()
    }

    /// Why a `let` binds a value this frame does not own. It asks
    /// [`Builder::owned_binding`]'s facts in the report's order: what the
    /// type releases first, then who owns the storage.
    fn report_reason(
        &self,
        rhs: &Rhs,
        ty: &Type,
        static_value: bool,
        mutable: bool,
        lends: bool,
    ) -> Option<NotOwned> {
        // A must-use type reaching a `let` is discharged on every path, so
        // "nothing reclaims it" is the wrong sentence about one.
        if self.proto.release_kind(ty).is_none() {
            return Some(match self.proto.linear_kind(ty) {
                Some(l) => NotOwned::MustUse(l),
                None => NotOwned::NoRelease {
                    heap: self.proto.owns_heap(ty),
                },
            });
        }
        // A `mut` slot is released by its final value.
        if static_value && !mutable {
            return Some(NotOwned::Static);
        }
        if lends {
            return Some(NotOwned::Borrow("a view into its argument".into()));
        }
        match rhs {
            Rhs::Read(_) => Some(NotOwned::Borrow("read out of a place that owns it".into())),
            Rhs::Val(Val::Name(m)) if self.body.names[m.index()].borrow => {
                Some(NotOwned::Borrow("a borrow of somebody else's value".into()))
            }
            _ => None,
        }
    }

    /// Whether a call's result points into one of its arguments, so the name
    /// bound to it is a borrow: a lending prelude row (`at`, `bytes`) or a
    /// projection an `impl` declares.
    fn lends(&self, e: &Expr) -> bool {
        match e {
            Expr::Call { name, args, .. } => {
                (self.lends_name(name) && !self.copies_a_part(e))
                    || self.hands_back_a_borrow(name, args)
            }
            _ => false,
        }
    }

    /// Whether a call whose result is its argument (`blackBox`,
    /// `movecheck::hands_back`) hands back a borrow: it does when the
    /// argument is a place read or a lending call. An owned temporary is
    /// taken instead, and the result owns it. Either way one release stands
    /// for the value.
    fn hands_back_a_borrow(&self, name: &str, args: &[Expr]) -> bool {
        vyrn_frontend::movecheck::hands_back(name)
            && args
                .first()
                .is_some_and(|a| is_place_read(a) || self.lends(a))
    }

    /// Whether a call by this name lends: `a[i]` and its seeded element row, a
    /// lending prelude row, a projection no function shadows. A call that
    /// hands its argument back depends on the argument ([`Self::lends`]).
    fn lends_name(&self, name: &str) -> bool {
        name == vyrn_frontend::project::AT
            || name == vyrn_frontend::project::ELEM
            || prelude::lends(name)
            || self.own.place_names.contains(name)
    }

    /// The member `name` of the protocol that `recv`'s bound names, where
    /// `recv` is a bounded type parameter of the body as written. The checker
    /// dispatched the call through that bound, not through the first
    /// protocol that declares `name`.
    fn protocol_member(&self, name: &str, recv: &Expr) -> Option<(MethodId, &'a MethodSig)> {
        let Type::Param(t) = node_ty(self.own, recv.id())? else {
            return None;
        };
        let bound = self.bounds?.get(&t)?;
        (self.program.protocols.iter().enumerate()).find_map(|(i, p)| {
            let j = (bound.contains(&p.name))
                .then(|| p.methods.iter().position(|m| m.name == name))??;
            let id = MethodId {
                protocol: i as u32,
                member: j as u32,
            };
            Some((id, &p.methods[j]))
        })
    }

    fn projection(&self, name: &str) -> Option<&'a Function> {
        self.program
            .impls
            .iter()
            .flat_map(|i| i.places.iter())
            .find(|p| p.name == name)
    }

    /// A projection's body, stated as rows at the access site. A
    /// projection is inlined where it is called, so its rows belong to the
    /// site. The tree is [`vyrn_frontend::project::site`]'s, the one the
    /// checker typed, so each row lands on the node an emitter asks about.
    ///
    /// Answers the yielded place, so `s[h].next` walks what `at` yields after
    /// its prologue. `None` leaves the site as it was, where:
    ///
    /// - no compile scope is open: outside one the expansion leaks its tree,
    ///   which the LSP would pay per keystroke;
    /// - no projection answers for the receiver's type;
    /// - the projection is optional, which
    ///   [`Builder::optional_if_let`] states;
    /// - the receiver has no recorded type.
    fn inlined(
        &mut self,
        method: &str,
        recv: &'a Expr,
        args: &'a [Expr],
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<Option<Place>, Gap> {
        if !self.program.expansions.is_shared() {
            return Ok(None);
        }
        let Ok(rty) = self.ty_of(recv) else {
            return Ok(None);
        };
        // By the receiver's type: two `impl`s may declare a projection of one
        // name.
        let p = match self.program.expansions.site(
            &self.program.impls,
            Some(&rty),
            method,
            recv,
            args,
            line,
        ) {
            Ok(Some(p)) => p,
            Ok(None) => return Ok(None),
            Err(e) => return gap_d("a projection this site cannot inline", &e, line),
        };
        for s in &p.prologue {
            self.stmt(s, out)?;
        }
        // The yield is a borrow of the receiver (`-> read T`), so read it
        // through `Builder::place`: `Builder::rhs`'s field arm would take a
        // heap field out of the receiver. `check_places` refuses a yield that
        // is no place, so reaching this gap is a defect in that rule.
        if !is_place_read(&p.place) {
            return gap_d("a projection whose yield is not a place", method, line);
        }
        self.place(&p.place, out).map(Some)
    }

    /// `if let Some(x) = s.tryAt(h)` over an optional projection,
    /// stated at the site. The body splits into four parts at a miss test
    /// ([`vyrn_frontend::project::optional_inline`]) and no `Option` exists on
    /// either path, so the site is a two-way branch on the miss test with the
    /// source's `else` on the true edge, not a switch.
    ///
    /// Answers whether this site is one, under [`Builder::inlined`]'s
    /// conditions.
    fn optional_if_let(
        &mut self,
        e: &'a Expr,
        sid: NodeId,
        out: &mut Vec<St>,
    ) -> Result<bool, Gap> {
        let Some((pattern, scrutinee, then_block, else_block)) = e.as_if_let() else {
            return Ok(false);
        };
        if !self.program.expansions.is_shared() {
            return Ok(false);
        }
        let line = e.line();
        let Expr::Call { name, args, .. } = scrutinee else {
            return Ok(false);
        };
        let Some(recv) = args.first() else {
            return Ok(false);
        };
        let Ok(rty) = self.ty_of(recv) else {
            return Ok(false);
        };
        let Some((_, f)) = self.program.impls.place(&rty, name) else {
            return Ok(false);
        };
        if !vyrn_frontend::project::is_optional(f) {
            return Ok(false);
        }
        let p = match self.program.expansions.optional_site(
            &self.program.impls,
            Some(&rty),
            name,
            recv,
            &args[1..],
            line,
        ) {
            Ok(Some(p)) => p,
            Ok(None) => return Ok(false),
            Err(e) => return gap_d("an optional projection this site cannot inline", &e, line),
        };
        let held = match recv {
            Expr::Var { name, .. } => self.lookup(name),
            _ => None,
        };
        if let Some(n) = held {
            self.frame.reading.push(n);
        }
        let r = self.optional_body(p, pattern, then_block, else_block, sid, name, line, out);
        if held.is_some() {
            self.frame.reading.pop();
        }
        r
    }

    /// [`Builder::optional_if_let`]'s four parts, once the site is one.
    #[allow(clippy::too_many_arguments)]
    fn optional_body(
        &mut self,
        p: &'a vyrn_frontend::project::OptionalProjection,
        pattern: &Pattern,
        then_block: &'a Block,
        else_block: &'a Block,
        sid: NodeId,
        name: &str,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<bool, Gap> {
        for s in &p.prologue {
            self.stmt(s, out)?;
        }
        let cond = self.read_val(&p.miss, out)?;
        // The true edge is the miss: the source's `else`, the plan's edge 1.
        let mut miss = Vec::new();
        self.block(else_block, &mut miss)?;
        self.edge_drops(sid, 1, &mut miss)?;
        let mut hit = Vec::new();
        let mark = self.frame.scope.len();
        for s in &p.hit {
            self.stmt(s, &mut hit)?;
        }
        // The binder borrows the yielded place (`-> read Option<T>`); read it
        // through [`Builder::place`] for [`Builder::inlined`]'s reason.
        if let Pattern::Variant(_, binds) = pattern {
            if let Some(bind) = binds.first() {
                if !is_place_read(&p.place) {
                    return gap_d("a projection whose yield is not a place", name, line);
                }
                let ty = self.ty_of(&p.place)?;
                let place = self.place(&p.place, &mut hit)?;
                let n = self.name(bind, ty, false, line);
                hit.push(St::Let(n, Rhs::Read(place)));
                self.frame.scope.push((bind.name.clone(), n));
            }
        }
        self.block(then_block, &mut hit)?;
        self.edge_drops(sid, 0, &mut hit)?;
        self.frame.scope.truncate(mark);
        out.push(St::If {
            cond,
            then: miss,
            els: hit,
            site: sid,
        });
        Ok(true)
    }

    /// Makes the join `res` of an `if` or a `match` a borrow when an arm
    /// yields one and no arm yields an owned value
    /// (`if c { parts[0] } else { "Bool" }`). Where an arm owns its value the
    /// join owns it, and the kernel refuses a borrowed arm's store (#518).
    fn join_borrows(&mut self, res: Name, yields: &[Val]) {
        let owned = |v: &Val| matches!(v, Val::Name(n) if self.body.names[n.index()].releases);
        if yields.iter().any(|v| self.borrows(v)) && !yields.iter().any(owned) {
            self.body.names[res.index()].releases = false;
            self.body.names[res.index()].borrow = true;
        }
    }

    /// Whether a value is a borrow.
    fn borrows(&self, v: &Val) -> bool {
        match v {
            Val::Name(n) => self.body.names[n.index()].borrow,
            Val::Lit(_) => false,
        }
    }

    /// The validated type the value `e` of `from` crosses into at a
    /// destination of `to`, where the core states the crossing as `to`'s
    /// constructor (`validate::required`). A borrowed heap value is left out:
    /// the constructor would take what a plain binding borrows.
    fn checked(&self, from: &Type, to: &Type, e: &Expr) -> Option<String> {
        vyrn_frontend::validate::required(from, to, self.proto.types())
            .filter(|_| !(self.owns(from) && (is_place_read(e) || self.lends(e))))
            .map(|d| d.name.clone())
    }

    /// Whether the checker proved `e` a value of `to`
    /// ([`vyrn_frontend::validate::proven`]), with this body's scope resolving
    /// a name. The emitter reads the answer as [`Callee::Proven`].
    fn proven(&self, e: &Expr, to: &Type) -> bool {
        let resolve = |x: &Expr| match x {
            Expr::Var { name, .. } => self
                .lookup(name)
                .map(|n| self.body.names[n.index()].ty.clone()),
            _ => None,
        };
        vyrn_frontend::validate::proven(e, to, self.proto.types(), &resolve)
    }

    /// [`Builder::checked`] where the checker proved the crossing.
    fn proven_crossing(&self, e: &Expr, to: &Type) -> Option<String> {
        let from = self.ty_of(e).ok()?;
        self.checked(&from, to, e).filter(|_| self.proven(e, to))
    }

    /// The value `e` takes at a destination of `to`: where the checker proved
    /// the crossing ([`Builder::proven_crossing`]), the validated type's
    /// constructor bound to a temporary; otherwise `e`'s own value, whose
    /// check the emitter runs. `to` is `None` where the builder knows no type.
    fn proven_val(
        &mut self,
        e: &'a Expr,
        to: Option<&Type>,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<Val, Gap> {
        match to.and_then(|to| self.proven_crossing(e, to)) {
            Some(t) => Ok(Val::Name(self.checked_temp(&t, e, line, out)?)),
            None => self.val(e, out),
        }
    }

    /// [`Builder::check`] bound to a temporary of the validated type `to`.
    /// A constructor hands its argument back, so over a literal the temporary
    /// is static data, as the literal is.
    fn checked_temp(
        &mut self,
        to: &str,
        value: &'a Expr,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<Name, Gap> {
        let rhs = self.check(to, value, line, out)?;
        let t = self.temp(Type::Named(to.to_string()), line);
        if over_a_literal(&rhs) {
            self.body.names[t.index()].releases = false;
            self.body.names[t.index()].not_owned = Some(NotOwned::Static);
        }
        self.bind(t, rhs, out);
        Ok(t)
    }

    /// The constructor of the validated type `to` over `value`.
    fn check(
        &mut self,
        to: &str,
        value: &'a Expr,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<Rhs, Gap> {
        let ty = Type::Named(to.to_string());
        self.call(to, std::slice::from_ref(value), line, Some(ty), None, out)
    }

    fn temp(&mut self, ty: Type, line: usize) -> Name {
        self.temps += 1;
        let owned = self.owns(&ty);
        let src = format!("@t{}", self.temps);
        self.name(&src, ty, owned, line)
    }

    /// Binds `n` to `rhs`, then releases the temporaries `rhs`'s reads queued:
    /// argument temporaries the caller frees, and String temporaries the
    /// reading site frees. They were the result's operands, so they
    /// go after it.
    fn bind(&mut self, n: Name, rhs: Rhs, out: &mut Vec<St>) {
        if matches!(rhs, Rhs::Prim(Op::Closure(_), ..)) {
            self.body.names[n.index()].closure_reads = self.frame.pending_closure.take();
        }
        let implicit = std::mem::take(&mut self.frame.pending_copy);
        self.body.names[n.index()].implicit_copy =
            implicit && rhs.copies(&self.body.names) == Some(Copied::Value);
        let owed = match (&rhs, self.frame.owed.take()) {
            (Rhs::Make(Ctor::Record(r, _), _), Some((to, line))) if *r == to => Some((to, line)),
            _ => None,
        };
        out.push(St::Let(n, rhs));
        // A validated record literal the checker did not prove is checked
        // whole once it is made: its constructor reads it.
        if let Some((to, line)) = owed {
            out.push(rule_check(to, n, line));
        }
        for t in std::mem::take(&mut self.frame.after_of_rhs) {
            out.push(St::Drop(t, Site::None, 0, None));
        }
    }

    fn lookup(&self, name: &str) -> Option<Name> {
        self.frame
            .scope
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, i)| *i)
    }

    /// The producer type of a node, before the destination's coercion (see
    /// [`Rhs`]). `None` where the checker typed no row: the judgment counts
    /// such a store rather than guessing.
    fn produced(&self, e: &Expr) -> Option<Type> {
        self.produced
            .get(&e.id())
            .cloned()
            // Fills only a projection expanded at the site, which has no row;
            // `ty_of` refuses everything else.
            .or_else(|| self.ty_of(e).ok())
    }

    fn ty_of(&self, e: &Expr) -> Result<Type, Gap> {
        match self.types.get(&e.id()) {
            Some(t) => Ok(t.clone()),
            // A call to a projection the checker expanded at the site
            // (`people.tryAt(h)`): its declared result, under the
            // receiver's type arguments.
            None if matches!(e, Expr::Call { name, args, .. }
                if !args.is_empty() && self.projection(name).is_some()) =>
            {
                let Expr::Call { name, args, .. } = e else {
                    unreachable!()
                };
                let p = self.projection(name).unwrap();
                let rty = self.ty_of(&args[0])?;
                Ok(match self.program.impls.place(&rty, name) {
                    Some((imp, f)) => vyrn_frontend::types::under_head(imp, &rty, &f.ret),
                    None => p.ret.clone(),
                })
            }
            None => gap_d(
                "an expression the checker did not type",
                &match e {
                    Expr::Var { name, .. } => format!("var {name}"),
                    Expr::Call { name, .. } => format!("call {name}"),
                    _ => expr_kind(e).to_string(),
                },
                e.line(),
            ),
        }
    }

    /// The releases the plan placed at one exit, as drops, in the plan's order.
    fn drops_at(&self, exit: Exit, site: NodeId, out: &mut Vec<St>) -> Result<(), Gap> {
        self.drops_at_but(exit, site, None, out)
    }

    /// [`Builder::drops_at`] with one binding left held: the place a `return`
    /// hands a read of ([`Builder::return_exit`]).
    fn drops_at_but(
        &self,
        exit: Exit,
        site: NodeId,
        keep: Option<Name>,
        out: &mut Vec<St>,
    ) -> Result<(), Gap> {
        let Some(rows) = self.placed.get(&(exit, site)) else {
            return Ok(());
        };
        for r in rows {
            match self.frame.by_binding.get(&r.binding) {
                // The core states no row for a name it does not own: a payload
                // binder of a non-consuming construct names its scrutinee's
                // payload, released once at the scrutinee. A consuming loop's
                // container keeps its row, because the row is the loop's take,
                // which the kernel judges.
                Some(n)
                    if !self.body.names[n.index()].releases
                        && !self.body.names[n.index()].for_consume => {}
                Some(n) if keep == Some(*n) => {}
                Some(n) => {
                    // The row's own set (a placer row), else the binding's.
                    let holes = if let Some(h) = &r.holes {
                        h.iter().map(|h| format!(".{h}")).collect()
                    } else {
                        self.body.names[n.index()].holes.clone()
                    };
                    out.push(St::Row {
                        name: *n,
                        holes,
                        exit,
                        site,
                    });
                }
                None => {
                    return gap_d(
                        "a placed release of a binding this slice did not name",
                        &r.name,
                        r.line as usize,
                    )
                }
            }
        }
        Ok(())
    }

    /// A `return`: the streams closed, the exit's releases, and the return.
    /// The releases skip the binding the returned value reads out of
    /// (`return d.s`), so the kernel refuses such a return as a return, not
    /// as a write around a live alias.
    fn return_exit(
        &mut self,
        v: Option<Val>,
        sid: NodeId,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<(), Gap> {
        self.leave_loops(out);
        self.drops_at_but(Exit::Return, sid, self.reads_out_of(&v), out)?;
        out.push(St::Return {
            value: v,
            site: sid,
            is_try: false,
            line,
        });
        Ok(())
    }

    /// The binding of this frame a returned place read reads out of.
    fn reads_out_of(&self, v: &Option<Val>) -> Option<Name> {
        let Some(Val::Name(n)) = v else { return None };
        let info = &self.body.names[n.index()];
        if !info.borrow {
            return None;
        }
        let path = info.path.as_deref()?;
        let end = path.find(['.', '[']).unwrap_or(path.len());
        self.lookup(&path[..end])
    }

    /// Lowers a `return` of an `if` or a `match` by ending each arm with the
    /// return, so the kernel judges each arm's value as returned, not as a
    /// store into a minted result no reader wrote.
    ///
    /// `Ok(false)` for any other shape. A `match` with a block arm
    /// is left whole: a block arm carries its own exits.
    fn return_through(
        &mut self,
        e: &'a Expr,
        sid: NodeId,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<bool, Gap> {
        match e {
            Expr::IfExpr {
                cond,
                then_branch,
                else_branch: Some(else_branch),
                line: at,
                id: _,
            } => {
                let site = e.id();
                let c = self.condition(cond, "if", *at, out)?;
                let mut t = Vec::new();
                self.arm_returns(then_branch, sid, line, &mut t)?;
                let mut f = Vec::new();
                self.arm_returns(else_branch, sid, line, &mut f)?;
                out.push(St::If {
                    cond: c,
                    then: t,
                    els: f,
                    site,
                });
                Ok(true)
            }
            Expr::Match {
                scrutinee,
                arms,
                line: mline,
                ..
            } if arms.iter().all(|a| matches!(a.body, ArmBody::Expr(_))) => {
                let sty = self.ty_of(scrutinee)?;
                let rt = vyrn_frontend::types::resolve(&sty, self.proto.types());
                let mid = e.id();
                let (sv, consuming) =
                    self.scrutinee(scrutinee, mid, Some(arms_span(*mline, arms)), out)?;
                let owns = self.owns_boxes(scrutinee, consuming);
                let held = self.frame.held.len();
                let mut core_arms = Vec::new();
                for (i, arm) in arms.iter().enumerate() {
                    self.frame.held.truncate(held);
                    let mut body = Vec::new();
                    let mark = self.frame.scope.len();
                    let binds = self.bind_pattern(
                        &arm.pattern,
                        &sty,
                        &rt,
                        consuming,
                        *mline,
                        borrow_root(&sv, owns),
                        &mut body,
                    )?;
                    let ArmBody::Expr(ae) = &arm.body else {
                        return gap("a block arm under a returned match", *mline);
                    };
                    let v = self.val(ae, &mut body)?;
                    let frees = self.arm_frees(mid, i as u32, &binds, &mut body);
                    self.edge_drops(mid, i as u32, &mut body)?;
                    // Every arm returns, so the scrutinee's release goes here:
                    // nothing reaches the statement after the switch.
                    self.drops_at(Exit::Scrutinee, mid, &mut body)?;
                    self.return_exit(Some(v), sid, line, &mut body)?;
                    self.frame.scope.truncate(mark);
                    core_arms.push(Arm {
                        binds,
                        frees: Some(frees),
                        body,
                        test: self.arm_test(&arm.pattern, &rt, *mline)?,
                        site: mid,
                        index: i as u32,
                    });
                }
                self.covers(&core_arms, &rt, *mline, out)?;
                out.push(St::Switch {
                    on: sv,
                    arms: core_arms,
                    consuming,
                    carries: true,
                    owns,
                    site: mid,
                    line: *mline,
                });
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// One arm of a returned `if` or `match`: its value, and the return that
    /// carries it out. A nested `if`/`match` carries the exit on down.
    fn arm_returns(
        &mut self,
        e: &'a Expr,
        sid: NodeId,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<(), Gap> {
        if self.return_through(e, sid, line, out)? {
            return Ok(());
        }
        let held = self.frame.held.len();
        let v = self.val(e, out)?;
        self.frame.held.truncate(held);
        self.return_exit(Some(v), sid, line, out)
    }

    fn block(&mut self, blk: &'a Block, out: &mut Vec<St>) -> Result<(), Gap> {
        self.block_with(blk, Vec::new(), out)
    }

    /// A source block, opened with `head` already in it: a `for` binds its
    /// variable inside the body's block, so the variable's scope ends at
    /// the block's site and a row for it can be keyed there.
    fn block_with(&mut self, blk: &'a Block, head: Vec<St>, out: &mut Vec<St>) -> Result<(), Gap> {
        let mark = self.frame.scope.len();
        let site = blk.id();
        let mut body = head;
        self.stmt_list(&blk.stmts, &mut body)?;
        self.drops_at(Exit::Block, site, &mut body)?;
        self.frame.scope.truncate(mark);
        out.push(St::Block {
            site,
            body,
            region: false,
        });
        Ok(())
    }

    /// The statements of a list.
    ///
    /// A run of statements that each store into a field of one record name
    /// with a `where` rule is a group ([`crate::typed::groups`]): the rule is
    /// checked once, after the run's last statement. A statement that ends
    /// the path ends the run with no check.
    fn stmt_list(&mut self, ss: &'a [Stmt], out: &mut Vec<St>) -> Result<(), Gap> {
        // Per open group: the record name, its type and the line of its last
        // statement.
        let mut open: Vec<(Name, String, usize)> = Vec::new();
        for s in ss {
            let (scope, at) = (self.frame.scope.len(), out.len());
            if let Err(g) = self.stmt(s, out) {
                // The checker typed an unknown name `Err` and went on, so a
                // gap may come before the builder meets it.
                let mut named = Vec::new();
                vyrn_frontend::ast::exprs_one(s, &mut |e, locals| {
                    let local = matches!(e, Expr::Var { name, .. } if locals.contains(name));
                    let typed = self.types.get(&e.id());
                    if !local && matches!(typed, None | Some(Type::Err)) {
                        named.extend(self.unknown_of(e));
                    }
                });
                self.body.refused.extend(named);
                if self.body.refused.is_empty() && self.body.mistyped.is_empty() {
                    return Err(g);
                }
                // The body is refused: the statements go, and a name one
                // binds is poisoned. The trap stands for them, so a `return`
                // among them is not read as falling through ([`returns`]).
                out.truncate(at);
                out.push(St::Trap);
                self.frame.scope.truncate(scope);
                if let Stmt::Let {
                    name,
                    line,
                    mutable,
                    ..
                } = s
                {
                    let n = self.name(name, Type::Err, false, *line);
                    self.body.names[n.index()].mutable = *mutable;
                    self.frame.scope.push((name.clone(), n));
                }
            }
            let members = self.grouped_in(&out[at..]);
            let (kept, closed): (Vec<_>, Vec<_>) =
                (open.into_iter()).partition(|(c, ..)| members.iter().any(|(m, _)| m == c));
            let checks = (closed.into_iter()).map(|(c, to, line)| rule_check(to, c, line));
            out.splice(at..at, checks);
            open = kept;
            let line = s.line();
            for (c, to) in members {
                match open.iter_mut().find(|(o, ..)| *o == c) {
                    Some(g) => g.2 = line,
                    None => open.push((c, to, line)),
                }
            }
            let ends = matches!(
                out.last(),
                Some(St::Return { .. } | St::Break { .. } | St::Continue { .. } | St::Trap)
            );
            if ends {
                open.clear();
            }
        }
        out.extend(
            open.into_iter()
                .map(|(c, to, line)| rule_check(to, c, line)),
        );
        Ok(())
    }

    /// The record names `rows` store into a field of as a group member
    /// ([`grouped`]), each once, with the record's type. The rows of a nested
    /// block or loop are left out: a block checks its own groups.
    fn grouped_in(&self, rows: &[St]) -> Vec<(Name, String)> {
        fn shallow<'s>(ss: &'s [St], out: &mut Vec<&'s St>) {
            for s in ss {
                out.push(s);
                if !matches!(s, St::Block { .. } | St::Loop { .. }) {
                    s.lists().for_each(|l| shallow(l, out));
                }
            }
        }
        let mut flat = Vec::new();
        shallow(rows, &mut flat);
        let mut out: Vec<(Name, String)> = Vec::new();
        for s in flat {
            crate::typed::row_stores(s, &self.body.names, &mut |place, _, _, _| {
                if let Some((c, to)) = group_of(self.proto, &self.body.names, place) {
                    if !out.iter().any(|(m, _)| *m == c) {
                        out.push((c, to));
                    }
                }
            });
        }
        out
    }

    /// A store into `name base.. leaf`, whole: the place is stated before the
    /// value, so an index of the path runs first and an element's bounds check
    /// runs at the store.
    ///
    /// A user container at the root yields the place of its first step from
    /// `atSet`: the checker expanded the store
    /// ([`vyrn_frontend::project::Expansions::store_index`]), whose prologue
    /// runs here and whose store names the place. The rest of the path and
    /// the value are the source's.
    ///
    /// The path's own indices below the root are bound to temps, root to leaf.
    /// Only a store to a whole name hands the old value back. A value built
    /// from the place's own value (`s.xs = s.xs.push(v)`) is moved out first,
    /// which leaves a hole the store fills, so a field or an element needs no
    /// hand-back at any depth.
    #[allow(clippy::too_many_arguments)]
    fn store(
        &mut self,
        sid: NodeId,
        name: &str,
        base: &'a [Step],
        leaf: &'a Step,
        value: &'a Expr,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<(), Gap> {
        let (root, rty) = self.named_place(name, line)?;
        let quoted = self.quoted(name, base);
        let expansion = self.program.expansions.stored_step(base, leaf);
        // The place the leaf writes into, and for `c[i] = v` on a user container
        // the place `atSet` yields, which is the element itself.
        let mut tmp = name.to_string();
        let (at, whole) = match expansion {
            Some((k, blk)) => {
                let Some((
                    Stmt::Store {
                        name: into,
                        base: b,
                        leaf: l,
                        ..
                    },
                    prologue,
                )) = blk.stmts.split_last()
                else {
                    return gap("an `atSet` expansion whose store is no place", line);
                };
                for s in prologue {
                    self.stmt(s, out)?;
                }
                // A second projected step is the expansion's own, which the
                // checker typed and expanded in turn.
                if self.program.expansions.stored_step(b, l).is_some() {
                    return self.store(sid, into, b, l, value, line, out);
                }
                let steps: Vec<At> = b.iter().chain([l]).map(Step::at).collect();
                // The steps up to the element `atSet` yields: the receiver's own
                // and the projection's. The expansion appends the source's steps
                // after the projected one.
                let Some(yielded) = steps.len().checked_sub(base.len() - k).map(|n| &steps[..n])
                else {
                    return gap("an `atSet` expansion shorter than the store", line);
                };
                let (eroot, ety) = self.named_place(into, line)?;
                tmp = into.clone();
                let (place, ty) = self.walk(eroot, ety, yielded, false, &mut tmp, line, out)?;
                match base.get(k + 1..) {
                    // The leaf is the projected step: the receiver's type is
                    // the one the leaf's index is judged against.
                    None => {
                        let at = self.path_ty(rty, &base[..k], line)?;
                        ((root, at), Some(place))
                    }
                    Some(rest) => {
                        let rest: Vec<At> = rest.iter().map(Step::at).collect();
                        let at = self.walk(place, ty, &rest, true, &mut tmp, line, out)?;
                        (at, None)
                    }
                }
            }
            None => {
                let steps: Vec<At> = base.iter().map(Step::at).collect();
                (
                    self.walk(root, rty, &steps, true, &mut tmp, line, out)?,
                    None,
                )
            }
        };
        let nested = !base.is_empty();
        match leaf {
            Step::Field(field) => self.set_field(at, &quoted, field, value, sid, line, out),
            Step::Index(index) => self.index_set(
                at, name, &quoted, index, value, sid, nested, whole, line, out,
            ),
        }
    }

    /// Binds the operand `e` of a store to a temp `name` and answers it.
    fn bind_temp(
        &mut self,
        name: &str,
        e: &'a Expr,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<Val, Gap> {
        self.bind_let(None, name, e, None, false, line, out)?;
        match self.lookup(name) {
            Some(n) => Ok(Val::Name(n)),
            None => gap_d("a store operand out of scope", name, line),
        }
    }

    /// The place `steps` name inside `place`, whose type is `ty`. Each index but
    /// a literal is bound to a temp `{tmp}[]idx`, so a step's index runs once,
    /// before the next step's; the last is read where it stands unless
    /// `bind_last`. `tmp` names the path so far.
    #[allow(clippy::too_many_arguments)]
    fn walk(
        &mut self,
        mut place: Place,
        mut ty: Type,
        steps: &[At<'a>],
        bind_last: bool,
        tmp: &mut String,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<(Place, Type), Gap> {
        for (k, step) in steps.iter().enumerate() {
            let next = self.step_ty(&ty, step, line)?;
            match *step {
                At::Field(field) => {
                    place = Place::Field(Box::new(place), field.to_string());
                    *tmp = format!("{tmp}.{field}[]");
                }
                At::Index(index) => {
                    let bound = (bind_last || k + 1 < steps.len())
                        && !matches!(index, Expr::Int(..))
                        && self.ty_of(index).is_ok_and(|t| !self.owns(&t));
                    let at = if bound {
                        self.bind_temp(&format!("{tmp}[]idx"), index, line, out)?
                    } else if self.is_map(&ty) {
                        self.val(index, out)?
                    } else {
                        self.read_val(index, out)?
                    };
                    *tmp = format!("{tmp}[]");
                    place = match self.is_map(&ty) {
                        true => Place::Key(Box::new(place), at),
                        false => Place::Elem(Box::new(place), at),
                    };
                }
            }
            ty = next;
        }
        Ok((place, ty))
    }

    /// The place `recv` names, which a call shrinks where it lies. A step through
    /// a user container is replaced by the place its `atSet` yields
    /// ([`vyrn_frontend::project::Expansions::modify_site`]), whose prologue runs
    /// first. The steps after it walk from the place it yields.
    fn modify_place(
        &mut self,
        recv: &'a Expr,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<(Place, Type), Gap> {
        let Some((_, root, steps)) = vyrn_frontend::parser::place_steps(recv) else {
            return gap("a removal whose receiver is no place", line);
        };
        let ty = |e: &Expr| self.ty_of(e).ok();
        let site = self
            .program
            .expansions
            .modify_site(&self.program.impls, recv, ty, line);
        let (place, ty, steps) = match site {
            Ok(Some((after, p))) => {
                for s in &p.prologue {
                    self.stmt(s, out)?;
                }
                let (place, ty) = self.modify_place(&p.place, line, out)?;
                (place, ty, &steps[steps.len() - after..])
            }
            Ok(None) => {
                let (place, ty) = self.named_place(root, line)?;
                (place, ty, &steps[..])
            }
            Err(e) => return gap_d("a projection this site cannot inline", &e, line),
        };
        self.walk(place, ty, steps, true, &mut root.to_string(), line, out)
    }

    /// The type of the place `step` names inside a place of type `ty`.
    fn step_ty(&mut self, ty: &Type, step: &At<'_>, line: usize) -> Result<Type, Gap> {
        match *step {
            At::Field(field) => self.field_ty(ty, field, line).inspect_err(|_| {
                self.refuse_field(ty, field, line);
            }),
            At::Index(_) => {
                // A user container's element is the place its `atSet` yields,
                // which only a store expanded at its projected step states
                // ([`Builder::store`]); elsewhere it is judged as the element
                // place it names.
                let next = match self.program.impls.place(ty, "atSet") {
                    Some((imp, f)) => vyrn_frontend::types::under_head(imp, ty, &f.ret),
                    None => self.elem_ty(ty, line)?,
                };
                // A map entry is no place: it reads as the `Option` a lookup
                // answers, which the leaf refuses.
                Ok(match self.is_map(ty) {
                    true => Type::option(next),
                    false => next,
                })
            }
        }
    }

    /// The type of the place `steps` name inside a place of type `ty`.
    fn path_ty(&mut self, mut ty: Type, steps: &[Step], line: usize) -> Result<Type, Gap> {
        for step in steps {
            ty = self.step_ty(&ty, &step.at(), line)?;
        }
        Ok(ty)
    }

    /// Records the refusal of a path through `field` where the type of the
    /// place has none: the typed judgment's, which a field read has no node
    /// to state.
    fn refuse_field(&mut self, ty: &Type, field: &str, line: usize) {
        let decls = self.proto.types();
        let sp = self.body.speech();
        let refusal = match vyrn_frontend::types::resolve(ty, decls) {
            Type::Err => return,
            Type::Record(_) => {
                let ty = sp.ty(ty).to_string();
                rule!(NoField, ty, field)
            }
            other => {
                let ([other], []) = sp.say([&other], []);
                rule!(FieldOnNonRecord, field, other)
            }
        };
        self.body.mistyped.push((line, refusal.render()));
    }

    /// The place `name base..` as the reader wrote it, as the checker quotes it
    /// (`ps[0]`, `r.xs`): the root as [`Builder::written`] spells it.
    fn quoted(&self, name: &str, base: &[Step]) -> String {
        vyrn_frontend::project::path_text(self.written(name), base)
    }

    fn stmt(&mut self, s: &'a Stmt, out: &mut Vec<St>) -> Result<(), Gap> {
        let held = std::mem::take(&mut self.frame.held);
        let outer = std::mem::replace(&mut self.frame.stmt, s.id());
        let r = self.stmt_rows(s, out);
        self.frame.stmt = outer;
        self.frame.held = held;
        r?;
        match self.frame.owed.take() {
            Some((to, line)) => gap_d("a check of a validated record no binding took", &to, line),
            None => Ok(()),
        }
    }

    /// The rows of `let name = value`, and the name in scope. `s` is the
    /// statement, which keys the binding's plan; `None` for a temp the builder
    /// binds itself ([`Builder::store_place`]).
    #[allow(clippy::too_many_arguments)]
    fn bind_let(
        &mut self,
        s: Option<&'a Stmt>,
        name: &str,
        value: &'a Expr,
        annotation: Option<&Type>,
        mutable: bool,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<(), Gap> {
        if let Some(vty) = node_ty(self.own, value.id()) {
            let decls = self.proto.types();
            let refusal = match annotation {
                Some(t) if !vyrn_frontend::types::coercible(&vty, t, decls) => {
                    let ([declared, vty], []) = self.body.speech().say([t, &vty], []);
                    Some(rule!(InitMismatch, name, declared, vty).render())
                }
                _ if vyrn_frontend::types::resolve(&vty, decls) == Type::Unit => {
                    Some(rule!(BindUnit, name).render())
                }
                _ => None,
            };
            self.body.mistyped.extend(refusal.map(|r| (line, r)));
        }
        let ty = self.ty_of(value)?;
        let check = annotation.and_then(|t| self.checked(&ty, t, value));
        let copied = check.is_none() && s.is_some_and(|s| self.copies(s, &ty));
        // Nothing may take module state: `let t = g`, `let t = consume g`
        // and `let t = consume g.f` bind a read of the place.
        let read = match value {
            Expr::Consume { place, .. } if self.in_module_state(place) => &**place,
            _ => value,
        };
        let global = self.in_module_state(read);
        if check.is_none()
            && !copied
            && (global || !matches!(read, Expr::Var { .. }))
            && is_place_read(read)
            && !self.forces(read)
        {
            let mark = self.frame.after.len();
            let place = self.place(read, out)?;
            let n = self.name(name, ty.clone(), false, line);
            let rhs = Rhs::Read(place);
            self.body.names[n.index()].not_owned =
                self.report_reason(&rhs, &ty, false, false, self.lends(read));
            if std::ptr::eq(read, value) {
                self.spell_take(read, n);
            }
            out.push(St::Let(n, rhs));
            self.release_receiver(read, out, true);
            self.drop_since(mark, out);
            self.grows(n, name);
            self.frame.scope.push((name.to_string(), n));
            self.keyed_binding(n, s, value);
            return Ok(());
        }
        // A crossing into a validated type is its constructor:
        // `let a: Age = n` is `let a = Age(n)`.
        let (rhs, ty) = match check {
            Some(to) => (self.check(&to, value, line, out)?, Type::Named(to)),
            None if copied => {
                let outer = std::mem::take(&mut self.frame.after);
                let rhs = self.copy_of(value, out);
                self.frame.after_of_rhs = std::mem::replace(&mut self.frame.after, outer);
                (rhs?, ty)
            }
            None => (self.rhs(value, out)?, ty),
        };
        // A literal, or a nullary constructor: nothing allocated it.
        let static_value = match &rhs {
            Rhs::Val(Val::Lit(l)) => !matches!(l, Lit::Opaque(_)),
            _ if over_a_literal(&rhs) => true,
            Rhs::Call {
                kind: Callee::Ctor,
                args,
                ..
            } if args.is_empty() => true,
            Rhs::Val(Val::Name(m)) => {
                matches!(self.body.names[m.index()].not_owned, Some(NotOwned::Static))
            }
            _ => false,
        };
        // A lending call binds a borrow whatever its type says; the
        // `Rhs` does not carry that. A copy lends nothing.
        let lends = !copied && self.lends(value);
        let owned = !lends && self.owned_binding(&rhs, &ty, static_value, mutable);
        let reason = self.report_reason(&rhs, &ty, static_value, mutable, lends);
        if owned {
            self.loop_alias(&rhs, line)?;
        }
        // Not owned is not borrowed: static data and a heapless value
        // are nobody's borrow.
        let borrow = !owned && (lends || matches!(&rhs, Rhs::Val(v) if self.borrows(v)));
        let n = self.name(name, ty, owned, line);
        self.body.names[n.index()].borrow = borrow && self.body.names[n.index()].heap;
        self.body.names[n.index()].not_owned = reason;
        self.record_fields(n, value);
        // `let t = s` on a `read` parameter makes `t` a second name
        // for it, with its words and its must-use take exception.
        if let Rhs::Val(Val::Name(m)) = &rhs {
            if self.body.names[n.index()].borrow {
                self.body.names[n.index()].borrow_kind =
                    self.body.names[m.index()].borrow_kind.clone();
                self.body.names[n.index()].must_use_param =
                    self.body.names[m.index()].must_use_param;
            }
        }
        self.bind(n, rhs, out);
        // The unnamed receiver of the part read: released after the
        // read where the plan says this frame owns it.
        if reads_a_part(value) {
            self.release_receiver(value, out, false);
        }
        self.grows(n, name);
        self.frame.scope.push((name.to_string(), n));
        self.keyed_binding(n, s, value);
        Ok(())
    }

    /// Keys a name by the `let` that binds it. A temp with no statement is
    /// keyed by its value's node: the emitter tells a binding from an operand
    /// temporary by the key.
    fn keyed_binding(&mut self, n: Name, s: Option<&Stmt>, value: &Expr) {
        match s {
            Some(s) => self.keyed_let(n, s),
            None => {
                self.keyed(n, value.id());
                self.body.names[n.index()].bound_by_let = true;
            }
        }
    }

    fn stmt_rows(&mut self, s: &'a Stmt, out: &mut Vec<St>) -> Result<(), Gap> {
        let sid = s.id();
        match s {
            Stmt::Let {
                name,
                value,
                line,
                ty: annotation,
                mutable,
                ..
            } => self.bind_let(
                Some(s),
                name,
                value,
                annotation.as_ref(),
                *mutable,
                *line,
                out,
            )?,
            Stmt::Assign {
                name,
                value,
                line,
                id: _,
            } => {
                if !self.known(name, *line, |name| rule!(AssignUnknown, name)) {
                    return Ok(());
                }
                let to = match self.lookup(name) {
                    Some(n) => Some(self.body.names[n.index()].ty.clone()),
                    None => self.named_place(name, *line).ok().map(|(_, t)| t),
                };
                if let (Some(to), Some(vty)) = (&to, node_ty(self.own, value.id())) {
                    if !vyrn_frontend::types::coercible(&vty, to, self.proto.types()) {
                        let ([to, vty], []) = self.body.speech().say([to, &vty], []);
                        let name = self.written(name);
                        let refusal = rule!(AssignMismatch, name, to, vty).render();
                        self.body.mistyped.push((*line, refusal));
                    }
                }
                let n = self.lookup(name);
                // A crossing into a validated type is its constructor.
                let check = match &to {
                    Some(to) => self.checked(&self.ty_of(value)?, to, value),
                    None => None,
                };
                let grown = match (n, &check) {
                    (Some(n), None)
                        if self.frame.region == 0 && self.body.names[n.index()].grows =>
                    {
                        crate::append::self_append_spine(name, value).map(|parts| (n, parts))
                    }
                    // Module state grows through a read of it, which the row
                    // names as its receiver.
                    (None, None)
                        if self.frame.region == 0
                            && self.own.accumulators.contains(name)
                            && vyrn_frontend::types::resolve(
                                &self.ty_of(value)?,
                                self.proto.types(),
                            ) == Type::Str =>
                    {
                        match crate::append::self_append_spine(name, value) {
                            Some(parts) => {
                                let mut root = value;
                                while let Expr::Binary { lhs, .. } = root {
                                    root = lhs;
                                }
                                let Val::Name(g) = self.global_read(root, name, *line, out)? else {
                                    return gap("a module-state read that names no value", *line);
                                };
                                self.body.names[g.index()].grows = true;
                                Some((g, parts))
                            }
                            None => None,
                        }
                    }
                    _ => None,
                };
                self.frame.rebinding = grown.is_none();
                let v = match (check, grown) {
                    (_, Some((n, parts))) => self.str_append(n, &parts, *line, out),
                    (Some(to), None) => self.checked_temp(&to, value, *line, out).map(Val::Name),
                    _ => self.val(value, out),
                };
                self.frame.rebinding = false;
                let mut v = v?;
                let ty = match n {
                    Some(n) => self.body.names[n.index()].ty.clone(),
                    None => self.ty_of(value)?,
                };
                // A slot that owns its value keeps owning: a borrow stored
                // into it is copied, as `let mut` binds one (`copies`), and
                // the store releases the old value. A `modify` parameter's
                // slot is the caller's, and the borrow's place is not. A
                // store of the slot's own name is not copied: the kernel
                // refuses `s = s` on a `modify` parameter, and a copy would
                // hide it.
                if let (Some(n), Val::Name(m)) = (n, &v) {
                    if self.slot_owns(n) && *m != n && self.body.names[m.index()].borrow {
                        let rhs = self.copy_rhs(v.clone(), value)?;
                        let t = self.temp(ty.clone(), *line);
                        self.bind(t, rhs, out);
                        v = Val::Name(t);
                    }
                }
                // The rule for a store to a name or module state: the plan
                // says whether it releases the old value. A value that
                // mentions the place may hand the old buffer back
                // (`xs = xs.push(v)`), so the release stands down, unless the
                // plan proved every mention a read argument that cannot hand
                // it back (`store_is_fresh`), or the value is a String
                // concatenation, which builds a fresh buffer (`s = s + x`).
                // The hand-back is read off the statement, so it is the
                // core's answer; the rest of what a store displaces is the
                // kernel's.
                let mentions = vyrn_frontend::ast::mentions_place(value, name);
                let fresh_str = self.fresh_str(&ty, value);
                let handed_back = mentions && !fresh_str && !self.store_is_fresh(value, name);
                let releases = !handed_back && self.own.placed.stores.contains_key(&sid);
                // Module state owns what it holds and nothing may `consume`
                // it, so a store into one releases what it replaces whenever
                // that owns heap.
                let (place, owes) = match n {
                    None => (Place::Global(name.clone()), self.owns(&ty)),
                    Some(n) => (Place::Name(n), self.slot_owns(n)),
                };
                // The hand-back comes before the place's obligation: a name
                // that owes no release still hands its buffer back, and the
                // word tells the two reasons apart.
                let old = if handed_back {
                    Old::Transferred
                } else if !owes {
                    Old::Nothing
                } else if releases {
                    Old::Released
                } else {
                    Old::Pending
                };
                out.push(St::Store {
                    place,
                    value: v,
                    old,
                    line: *line,
                    site: Site::Node(sid),
                    releases,
                    holes: if releases {
                        (self.own.placed.stores.get(&sid).cloned()).unwrap_or_default()
                    } else {
                        Vec::new()
                    },
                });
                // [`Builder::str_append`] queues its operand temporaries so the
                // store stays next to its row.
                for t in std::mem::take(&mut self.frame.after_of_rhs) {
                    out.push(St::Drop(t, Site::None, 0, None));
                }
            }
            Stmt::Store {
                name,
                base,
                leaf,
                value,
                line,
                id: _,
            } => {
                let known = match base.first().unwrap_or(leaf) {
                    Step::Field(_) => {
                        self.known(name, *line, |name| rule!(FieldAssignUnknown, name))
                    }
                    Step::Index(_) => {
                        self.known(name, *line, |name| rule!(IndexAssignUnknown, name))
                    }
                };
                if !known {
                    return Ok(());
                }
                self.store(sid, name, base, leaf, value, *line, out)?;
            }
            Stmt::Return { value, line, id: _ } => {
                let vty = match value {
                    Some(e) => node_ty(self.own, e.id()),
                    None => Some(Type::Unit),
                };
                if let (Some(vty), Some(ret)) = (vty, &self.frame.ret) {
                    if !vyrn_frontend::types::coercible(&vty, ret, self.proto.types()) {
                        let ([ret, vty], []) = self.body.speech().say([ret, &vty], []);
                        let refusal = rule!(ReturnMismatch, ret, vty).render();
                        self.body.mistyped.push((*line, refusal));
                    }
                }
                if let Some(e) = value {
                    if self.return_through(e, sid, *line, out)? {
                        return Ok(());
                    }
                }
                let v = match (value, self.frame.ret.clone()) {
                    (Some(e), Some(r)) => Some(self.proven_val(e, Some(&r), *line, out)?),
                    (Some(e), None) => Some(self.val(e, out)?),
                    (None, _) => None,
                };
                self.return_exit(v, sid, *line, out)?;
            }
            Stmt::Break { line, id: _ } => {
                if let Some(Some(u)) = self.frame.walks.last().cloned() {
                    self.release_unreached(&u, out);
                }
                self.drops_at(Exit::Break, sid, out)?;
                out.push(St::Break {
                    site: sid,
                    line: *line,
                });
            }
            Stmt::Continue { line, id: _ } => {
                self.drops_at(Exit::Continue, sid, out)?;
                out.push(St::Continue {
                    site: sid,
                    line: *line,
                });
            }
            Stmt::If {
                cond,
                then_block,
                else_block,
                line,
                id: _,
            } => {
                let c = self.condition(cond, "if", *line, out)?;
                let mut t = Vec::new();
                self.block(then_block, &mut t)?;
                self.edge_drops(sid, 0, &mut t)?;
                let mut e = Vec::new();
                if let Some(blk) = else_block {
                    self.block(blk, &mut e)?;
                }
                self.edge_drops(sid, 1, &mut e)?;
                out.push(St::If {
                    cond: c,
                    then: t,
                    els: e,
                    site: sid,
                });
            }
            Stmt::While {
                cond,
                body,
                line,
                id: _,
            } => {
                let mut l = Vec::new();
                let c = self.condition(cond, "while", *line, &mut l)?;
                l.push(St::If {
                    cond: c,
                    then: Vec::new(),
                    els: vec![St::Break {
                        site: NodeId::NONE,
                        line: 0,
                    }],
                    site: NodeId::NONE,
                });
                self.frame.loop_marks.push(self.body.names.len());
                self.frame.walks.push(None);
                let r = self.block(body, &mut l);
                self.frame.walks.pop();
                self.frame.loop_marks.pop();
                r?;
                self.hoist_headers(&mut l, *line, out);
                out.push(St::Loop { body: l, site: sid });
            }
            Stmt::ForIn {
                var,
                iter,
                body,
                line,
                consuming,
                col: _,
                id: _,
            } => {
                let ity = self.ty_of(iter)?;
                // A user container's element is what its `nth` projection
                // yields. A `for` walks no map.
                let elem = self
                    .elem_ty(&ity, *line)
                    .ok()
                    .filter(|_| !self.is_map(&ity));
                let projected = elem.is_none();
                let Some(ety) = elem.or_else(|| self.projected_elem(&ity)) else {
                    let t = vyrn_frontend::types::resolve(&ity, self.proto.types());
                    if t != Type::Err {
                        let ([t], []) = self.body.speech().say([&t], []);
                        let refusal = rule!(ForNeedsIterable, t).render();
                        self.body.mistyped.push((*line, refusal));
                    }
                    return gap("a `for` over what no loop walks", *line);
                };
                if *consuming {
                    take_names_a_place(iter, &self.own.place_names, *line, true)?;
                }
                // `owner` is the name the element rule below asks about: the
                // named container where the loop borrows it.
                let mut owner = None;
                let it = match iter {
                    Expr::Var { name, .. } if self.lookup(name).is_some() => {
                        let n = self.lookup(name).unwrap();
                        let pulled = matches!(
                            vyrn_frontend::types::resolve(&ity, &self.proto.types()),
                            Type::Stream(_)
                        );
                        if *consuming {
                            let t = self.temp(ity.clone(), *line);
                            out.push(St::Let(t, Rhs::Val(Val::Name(n))));
                            self.keyed(t, sid);
                            t
                        } else if pulled {
                            // A stream is pulled to its end and closed by the
                            // loop through its own name (below).
                            n
                        } else {
                            // The loop reads the container through a borrow,
                            // so the kernel refuses a store over the name in
                            // the body (`ys = []` would free the buffer the
                            // loop still walks).
                            owner = Some(n);
                            let t = self.borrow_name(iter, ity.clone(), *line);
                            self.body.names[t.index()].walked = Some(Walk::For);
                            out.push(St::Let(t, Rhs::Read(Place::Name(n))));
                            t
                        }
                    }
                    _ if !*consuming && is_place_read(iter) && !self.forces(iter) => {
                        // `for p in e.path`: the loop walks a container
                        // somebody else owns.
                        let place = self.place(iter, out)?;
                        // No drain encloses the receiver temporary, so it
                        // stays held and the judgment sees it.
                        self.frame.pending_receiver = None;
                        let t = self.borrow_name(iter, ity.clone(), *line);
                        self.body.names[t.index()].walked = Some(Walk::For);
                        out.push(St::Let(t, Rhs::Read(place)));
                        t
                    }
                    _ => {
                        match self.val(iter, out)? {
                            // The construct owns the temporary; the plan keys
                            // its release rows by the statement.
                            Val::Name(t) => {
                                self.keyed(t, sid);
                                t
                            }
                            // A String literal is static: its name owns nothing.
                            lit => {
                                let t = self.name("@lit", ity.clone(), false, *line);
                                out.push(St::Let(t, Rhs::Val(lit)));
                                t
                            }
                        }
                    }
                };
                // Whatever spelling brought the container here, the loop took
                // it, and a refusal about the take names the loop.
                if *consuming {
                    self.body.names[it.index()].for_consume = true;
                }
                let streaming = matches!(
                    vyrn_frontend::types::resolve(&ity, self.proto.types()),
                    Type::Stream(_)
                );
                if streaming {
                    self.frame.stream_loops.push(it);
                }
                // `for x in xs` walks a named index: the length read once
                // before the loop, and a counter from zero that steps right
                // after the element is read, so a `continue` needs no step. A
                // stream is pulled and has neither.
                let counter = if streaming {
                    None
                } else {
                    let n = self.temp(Type::Int, *line);
                    let len = self.length_of(it, &ity, *line)?;
                    out.push(St::Let(n, len));
                    let i = self.temp(Type::Int, *line);
                    out.push(St::Let(i, Rhs::Val(Val::Lit(Lit::Int(0)))));
                    Some((n, i))
                };
                let mut l = Vec::new();
                // The `if .. else break` a `while` has at its top. Without it
                // the path after the `for` is dead to the judgment, and the
                // placer rewrote hole sets from the other join edge alone
                // (`std/vyx`'s `vyxMergeImports`).
                let (cond, index) = match counter {
                    Some((n, i)) => {
                        let c = self.temp(Type::Bool, *line);
                        l.push(St::Let(
                            c,
                            Rhs::Prim(
                                Op::Bin(BinOp::Lt),
                                vec![Val::Name(i), Val::Name(n)],
                                Some(Type::Bool),
                            ),
                        ));
                        (Val::Name(c), Val::Name(i))
                    }
                    // A stream is pulled: one call answers whether an element
                    // came, and its name stands for the element below.
                    None => {
                        let c = self.temp(Type::Bool, *line);
                        l.push(St::Let(
                            c,
                            Rhs::Call {
                                callee: "@pull".into(),
                                args: vec![(Arg::Val(Val::Name(it)), Capability::Modify)],
                                write_back: false,
                                kind: Callee::Reserved,
                                ret: Some(Type::Bool),
                                solved: Vec::new(),
                                targets: Vec::new(),
                            },
                        ));
                        (Val::Name(c), Val::Name(c))
                    }
                };
                l.push(St::If {
                    cond,
                    then: Vec::new(),
                    els: vec![St::Break {
                        site: NodeId::NONE,
                        line: 0,
                    }],
                    site: NodeId::NONE,
                });
                // Each turn owns its element where the element type owns heap
                // and either the container is a stream (a pulled element has
                // no other owner), or the variable is handed on in the body
                // ([`last_owner`], `Cand::Elem`) out of an unnamed container
                // this frame owns. A named container outlives the loop, so
                // `for r in ns` only borrows; a lender's result is somebody
                // else's buffer.
                let ekey = body.id();
                let ic = &self.body.names[owner.unwrap_or(it).index()];
                let loops_alone = !ic.borrow && !ic.bound_by_let;
                // A projection yields a place in the container, never its own
                // element.
                let owned = self.owns(&ety)
                    && !projected
                    && (streaming || loops_alone && self.seed.contains(&ekey));
                // Where every element left through the variable, the
                // container's release frees the buffer alone (field 0 of a
                // growable array's triple; other containers have no such
                // buffer). A wrong answer here frees somebody else's storage.
                let mut unreached = None;
                if owned
                    && matches!(
                        vyrn_frontend::types::resolve(&ity, self.proto.types()),
                        Type::Array(_)
                    )
                {
                    self.body.loop_buffers.push(sid);
                    unreached = counter.map(|(n, i)| Unreached {
                        it,
                        i,
                        n,
                        elem: ety.clone(),
                        line: *line,
                    });
                }
                // Before the variable: each turn binds its own element, so it
                // may leave a join arm; the container, below the mark, may not.
                self.frame.loop_marks.push(self.body.names.len());
                let x = self.name(var, ety, owned, *line);
                self.body.cands.push((ekey, x, Cand::Elem));
                // The container outlives the loop, so a refusal names the
                // variable as a loop variable, as the checker does. A projected
                // element is refused as the place it is: `consume` cannot hand
                // it over.
                if !*consuming && !projected && self.body.names[x.index()].borrow {
                    let of = vyrn_frontend::ast::place_path(iter)
                        .map(|(r, _)| r)
                        .unwrap_or_default();
                    self.body.names[x.index()].loop_var = Some(of);
                }
                // The variable has no node of its own; the body that binds it
                // keys it.
                self.keyed(x, ekey);
                let mut head = Vec::new();
                let place = match counter {
                    Some((_, i)) if projected => {
                        self.for_element(iter, &ity, it, i, *line, &mut head)?
                    }
                    _ => Place::Elem(Box::new(Place::Name(it)), index),
                };
                head.push(St::Let(x, Rhs::Read(place)));
                if let Some((_, i)) = counter {
                    self.step(i, &mut head);
                }
                let mark = self.frame.scope.len();
                self.frame.scope.push((var.clone(), x));
                self.frame.walks.push(unreached);
                let r = self.block_with(body, head, &mut l);
                self.frame.walks.pop();
                self.frame.loop_marks.pop();
                r?;
                self.frame.scope.truncate(mark);
                out.push(St::Loop { body: l, site: sid });
                if streaming {
                    self.frame.stream_loops.pop();
                    // Pulled to its end or left by a `break`, the stream is
                    // closed here by its last owner, the loop. A binding's
                    // stream is always disposed of here, so a later use is a
                    // second disposal ([`Body::owes`]).
                    let owed = self.body.owes(it).is_some();
                    if self.stream_owed(it) && (owed || self.taken_by_loop(it, sid)) {
                        out.push(St::Drop(it, Site::None, 0, None));
                    }
                } else if *consuming && self.taken_by_loop(it, sid) {
                    // The loop is the container's last owner and releases it,
                    // keyed by the loop so emitters read the judgment rather
                    // than the source's `consume`.
                    out.push(St::Drop(it, Site::Node(sid), 0, None));
                }
                self.drops_at(Exit::Scrutinee, sid, out)?;
            }
            Stmt::Drop { name, line, id: _ } => {
                let Some(n) = self.lookup(name) else {
                    self.body.unbound_drops.push((name.clone(), *line));
                    return Ok(());
                };
                // An ordinary drop inside a `region` too: `free` refuses an
                // arena block by its class word.
                out.push(St::Drop(n, Site::Node(sid), *line, None));
            }
            // An unaudited build states neither the audit hook nor its operand;
            // `p + 8` alone costs four instructions per allocation.
            Stmt::Expr(Expr::Call { name, .. }, _)
                if vyrn_frontend::loader::audit_hook(name)
                    && !vyrn_frontend::loader::audit_build(self.program.host.gen) => {}
            Stmt::Expr(e, _) if self.optional_if_let(e, sid, out)? => {}
            // A part read as a statement takes nothing: the part stays in its
            // place, and an unnamed receiver is released whole.
            Stmt::Expr(e, _) if reads_a_part(e) && self.deferred_of(e).is_none() => {
                let mark = self.frame.after.len();
                let rhs = Rhs::Read(self.place(e, out)?);
                out.push(St::Do {
                    rhs,
                    line: e.line(),
                    site: sid,
                });
                if let Some((r, _, malloc)) = self.frame.pending_receiver.take() {
                    self.drop_receiver(r, malloc, Vec::new(), out);
                }
                self.drop_since(mark, out);
            }
            Stmt::Expr(e, _) => {
                let ty = self.ty_of(e).unwrap_or(Type::Unit);
                let rhs = self.rhs(e, out)?;
                if self.owns(&ty) || self.frame.owed.is_some() {
                    let owns = self.owns(&ty);
                    let t = self.temp(ty, e.line());
                    self.bind(t, rhs, out);
                    if owns && self.discards(e) {
                        out.push(St::Drop(t, Site::Node(sid), 0, None));
                    }
                } else {
                    // A bare value does nothing: a Unit `match` or `if` yields
                    // a join no arm writes.
                    if !matches!(rhs, Rhs::Val(_)) {
                        out.push(St::Do {
                            rhs,
                            line: e.line(),
                            site: sid,
                        });
                    }
                    for t in std::mem::take(&mut self.frame.after_of_rhs) {
                        out.push(St::Drop(t, Site::None, 0, None));
                    }
                }
            }
            // The arena owns only what `direct.rs`'s `arena_route` routes into
            // it, and the closing brace is the runtime's, so the body is an
            // ordinary block here.
            Stmt::Region { body, .. } => {
                self.frame.region += 1;
                let r = self.block(body, out);
                self.frame.region -= 1;
                r?;
                if let Some(St::Block { region, .. }) = out.last_mut() {
                    *region = true;
                }
            }
        }
        Ok(())
    }

    /// Whether a statement's unbound value is this frame's to release right
    /// after the statement. The caller has checked that the type owns heap.
    /// Excluded: a lending call, and a `panic`, which never returns.
    fn discards(&self, e: &Expr) -> bool {
        !matches!(e, Expr::Call { name, .. } if vyrn_frontend::ast::is_panic(name))
            && !self.lends(e)
    }

    /// Marks `n`, bound by a `let` of `name`, as a String accumulator where
    /// the whitelist admits the name.
    fn grows(&mut self, n: Name, name: &str) {
        let info = &mut self.body.names[n.index()];
        info.grows = self.frame.appends.contains(name)
            && vyrn_frontend::types::resolve(&info.ty, self.proto.types()) == Type::Str;
    }

    /// `s = s + a + b` on an accumulator: one `@strAppend` row that reads `s`
    /// and each part in written order; the store after it puts the result
    /// back into `s`. The parts' temporaries stay queued until the caller has
    /// pushed the store.
    fn str_append(
        &mut self,
        s: Name,
        parts: &[&'a Expr],
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<Val, Gap> {
        let outer = std::mem::take(&mut self.frame.after);
        self.frame.drain += 1;
        let mut args = vec![(Arg::Val(Val::Name(s)), Capability::Read)];
        let read = parts.iter().try_for_each(|p| {
            args.push((
                Arg::Val(self.read_arg(p, out, "@concat", 1)?),
                Capability::Read,
            ));
            Ok(())
        });
        self.frame.drain -= 1;
        self.frame.after_of_rhs = std::mem::replace(&mut self.frame.after, outer);
        read?;
        let t = self.temp(Type::Str, line);
        out.push(St::Let(
            t,
            Rhs::Call {
                callee: "@strAppend".into(),
                args,
                write_back: false,
                kind: Callee::Reserved,
                ret: Some(Type::Str),
                solved: Vec::new(),
                targets: Vec::new(),
            },
        ));
        Ok(Val::Name(t))
    }

    /// Whether the store value is a String concatenation, which builds a fresh
    /// buffer, so `s = s + x` displaces the old one rather than handing it
    /// back.
    fn fresh_str(&self, ty: &Type, value: &Expr) -> bool {
        matches!(
            vyrn_frontend::types::resolve(ty, self.proto.types()),
            Type::Str
        ) && matches!(
            value,
            Expr::Binary {
                op: vyrn_frontend::ast::BinOp::Add,
                ..
            }
        )
    }

    fn old_for(&self, ty: &Type, releases: bool) -> Old {
        if !self.owns(ty) {
            Old::Nothing
        } else if releases {
            Old::Released
        } else {
            // The kernel answers over the root, the one place a sub-place's
            // ownership can be read from.
            Old::Pending
        }
    }

    /// Releases the payload binders the kernel found still held where this
    /// arm ends. The first build states none, so [`crate::kernel::placement`]
    /// reports every held binder, and the second build reads the rows back
    /// out of [`Ownership::placed`].
    fn arm_frees(
        &mut self,
        site: NodeId,
        arm: u32,
        binds: &[Name],
        out: &mut Vec<St>,
    ) -> Vec<Name> {
        let mut frees: Vec<Name> = Vec::new();
        let Some(rows) = self.own.placed.arms.get(&(site, arm)).cloned() else {
            return frees;
        };
        for b in binds {
            let src = self.body.names[b.index()].source.clone();
            if let Some((_, holes)) = rows.iter().find(|(n, _)| *n == src) {
                self.body.names[b.index()].holes = holes.iter().map(|h| format!(".{h}")).collect();
                out.push(St::Drop(*b, Site::None, 0, None));
                frees.push(*b);
            }
        }
        frees
    }

    /// Whether the stream `it` a `for` walks is this frame's to close: one it
    /// holds, or a parameter, which carries the obligation into the callee
    /// ([`NameInfo::must_use_param`]).
    fn stream_owed(&self, it: Name) -> bool {
        let info = &self.body.names[it.index()];
        info.releases || info.must_use_param
    }

    /// The rows a `return` or a `?` runs for every enclosing `for`, innermost
    /// first: the elements no turn reached, then the stream it walks, closed.
    fn leave_loops(&mut self, out: &mut Vec<St>) {
        for u in self.frame.walks.clone().iter().rev().flatten() {
            self.release_unreached(u, out);
        }
        for it in self.frame.stream_loops.iter().rev() {
            if self.stream_owed(*it) {
                out.push(St::Drop(*it, Site::None, 0, None));
            }
        }
    }

    /// Releases the elements of `u`'s container from its counter to its
    /// length, which no turn bound: the rows a `return`, a `?` or a `break`
    /// runs before its own. The element the turn bound is the body's.
    fn release_unreached(&mut self, u: &Unreached, out: &mut Vec<St>) {
        let c = self.temp(Type::Bool, u.line);
        let mut l = vec![
            St::Let(
                c,
                Rhs::Prim(
                    Op::Bin(BinOp::Lt),
                    vec![Val::Name(u.i), Val::Name(u.n)],
                    Some(Type::Bool),
                ),
            ),
            St::If {
                cond: Val::Name(c),
                then: Vec::new(),
                els: vec![St::Break {
                    site: NodeId::NONE,
                    line: 0,
                }],
                site: NodeId::NONE,
            },
        ];
        let e = self.temp(u.elem.clone(), u.line);
        l.push(St::Let(
            e,
            Rhs::Read(Place::Elem(Box::new(Place::Name(u.it)), Val::Name(u.i))),
        ));
        self.step(u.i, &mut l);
        l.push(St::Drop(e, Site::None, 0, None));
        out.push(St::Loop {
            body: l,
            site: NodeId::NONE,
        });
    }

    /// Rule N: the drops one edge of a join owes.
    fn edge_drops(&mut self, join: NodeId, edge: u32, out: &mut Vec<St>) -> Result<(), Gap> {
        let Some(ers) = self.own.placed.edges.get(&join).cloned() else {
            return Ok(());
        };
        for (name, e, holes) in &ers {
            if *e != edge {
                continue;
            }
            // `d.line`: a sub-place the other edge took, released on this one
            // as a take into a temporary dropped at once, so the kernel sees
            // the hole.
            let mut parts = name.split('.');
            let root = parts.next().unwrap_or_default();
            let Some(n) = self.lookup(root) else {
                return gap_d("an edge release of a name out of scope", name, 0);
            };
            let mut place = Place::Name(n);
            let mut ty = self.body.names[n.index()].ty.clone();
            let mut sub = false;
            for f in parts {
                ty = self.field_ty(&ty, f, 0)?;
                place = Place::Field(Box::new(place), f.to_string());
                sub = true;
            }
            let at = Site::Edge(join, edge);
            if sub {
                let t = self.temp(ty, self.body.names[n.index()].line);
                // Spelled as the sub-place, so a reader gets the row's name.
                self.body.names[t.index()].source = name.clone();
                out.push(St::Let(t, Rhs::Take(place)));
                out.push(St::Drop(t, at, 0, None));
            } else {
                let holes =
                    (!holes.is_empty()).then(|| holes.iter().map(|h| format!(".{h}")).collect());
                out.push(St::Drop(n, at, 0, holes));
            }
        }
        Ok(())
    }

    /// A store into `field` of the place `base`, a part of a root that the
    /// reader wrote as `quoted`.
    fn set_field(
        &mut self,
        base: (Place, Type),
        quoted: &str,
        field: &str,
        value: &'a Expr,
        sid: NodeId,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<(), Gap> {
        self.field_store(&base.1, quoted, field, value, line)?;
        let (base, bty) = base;
        let fty = self.field_ty(&bty, field, line)?;
        let v = self.proven_val(value, Some(&fty), line, out)?;
        let releases = self.own.placed.stores.contains_key(&sid);
        out.push(St::Store {
            place: Place::Field(Box::new(base), field.to_string()),
            value: v,
            old: self.old_for(&fty, releases),
            line,
            site: Site::Node(sid),
            releases,
            holes: if releases {
                (self.own.placed.stores.get(&sid).cloned()).unwrap_or_default()
            } else {
                Vec::new()
            },
        });
        Ok(())
    }

    /// The rules of a store into `name.field`, as the reader wrote `name`, whose
    /// place has type `bty`: the place has the field, and the field takes the
    /// value. A root without the
    /// field is a refused gap. A validated record's field is `typed::stores`'s.
    fn field_store(
        &mut self,
        bty: &Type,
        name: &str,
        field: &str,
        value: &Expr,
        line: usize,
    ) -> Result<(), Gap> {
        let decls = self.proto.types();
        let fields = vyrn_frontend::types::record_fields(bty, decls);
        let refusal = match bty {
            Type::Err => return Ok(()),
            _ => match fields
                .as_ref()
                .map(|fs| fs.iter().find(|f| f.name == field))
            {
                None => rule!(NotRecordNoField, name, field).render(),
                Some(None) => rule!(RecordNoField, name, field).render(),
                Some(Some(f)) => {
                    let fty = &f.ty;
                    let Some(vty) = node_ty(self.own, value.id()) else {
                        return Ok(());
                    };
                    let validated = matches!(fty, Type::Named(n)
                        if decls.get(n).is_some_and(|d| d.predicate.is_some()));
                    let fits = match validated {
                        true => vyrn_frontend::types::assignable(&vty, fty, decls),
                        false => vyrn_frontend::types::coercible(&vty, fty, decls),
                    };
                    if fits {
                        return Ok(());
                    }
                    let ([fty, vty], []) = self.body.speech().say([fty, &vty], []);
                    let refusal = match validated {
                        true => rule!(FieldValidated, field, fty),
                        false => rule!(FieldMismatch, field, fty, vty),
                    }
                    .render();
                    self.body.mistyped.push((line, refusal));
                    return Ok(());
                }
            },
        };
        self.body.mistyped.push((line, refusal));
        gap("a store into a field its root has not", line)
    }

    /// The rules of a store into `name[index]`, as the reader wrote `name`,
    /// whose place has type `bty`: the place is a container, the index is its
    /// key type, and the element takes the value. A root that is no container is a refused gap.
    fn index_store(
        &mut self,
        bty: &Type,
        name: &str,
        index: &Expr,
        value: &Expr,
        line: usize,
    ) -> Result<(), Gap> {
        let decls = self.proto.types();
        let coercible = |a: &Type, b: &Type| vyrn_frontend::types::coercible(a, b, decls);
        let (ity, vty) = (node_ty(self.own, index.id()), node_ty(self.own, value.id()));
        let sp = self.body.speech();
        let refusal = match vyrn_frontend::types::resolve(bty, decls) {
            Type::Err => None,
            Type::Map(key, val) => {
                let k = ity.map(|t| vyrn_frontend::types::resolve(&t, decls));
                match (k, vty) {
                    // Both at their base, as a lookup takes its key.
                    (Some(k), _)
                        if k != Type::Err
                            && !coercible(&k, &vyrn_frontend::types::resolve(&key, decls)) =>
                    {
                        let ([key, k], []) = sp.say([&key, &k], []);
                        Some(rule!(MapStoreKey, name, key, k))
                    }
                    (_, Some(v)) if !coercible(&v, &val) => {
                        let ([val, v], []) = sp.say([&val, &v], []);
                        Some(rule!(MapStoreValue, name, val, v))
                    }
                    _ => None,
                }
            }
            shape => {
                let (key, elem) = match shape.elem() {
                    Some(e) => (Type::Int, e.clone()),
                    None => match self.program.impls.place(bty, "atSet") {
                        Some((imp, f)) => (
                            (f.params.get(1)).map_or(Type::Int, |p| {
                                vyrn_frontend::types::under_head(imp, bty, &p.ty)
                            }),
                            vyrn_frontend::types::under_head(imp, bty, &f.ret),
                        ),
                        None => {
                            let ([other], []) = sp.say([&shape], []);
                            let refusal = rule!(IndexStoreNoContainer, name, other).render();
                            self.body.mistyped.push((line, refusal));
                            return gap("a store into an element of what has none", line);
                        }
                    },
                };
                let i = ity.filter(|i| {
                    !coercible(i, &key) && vyrn_frontend::types::resolve(i, decls) != Type::Err
                });
                match (i, vty) {
                    (Some(i), _) if key == Type::Int => {
                        let ([i], []) = sp.say([&i], []);
                        Some(rule!(ArrayIndexType, i))
                    }
                    (Some(i), _) => {
                        let ([key, i], []) = sp.say([&key, &i], []);
                        Some(rule!(IndexStoreKey, name, key, i))
                    }
                    (None, Some(v)) if !coercible(&v, &elem) => {
                        let ([elem, v], []) = sp.say([&elem, &v], []);
                        Some(rule!(ElementMismatch, name, elem, v))
                    }
                    _ => None,
                }
            }
        };
        self.body
            .mistyped
            .extend(refusal.map(|r| (line, r.render())));
        Ok(())
    }

    /// A store into the element or the entry of the place `base` at `index`, a
    /// part of the root `name` that the reader wrote as `quoted`. `whole` is
    /// the place `atSet` yields where `index` is a user container's own;
    /// without it, a user container below the root (`nested`) is a gap.
    #[allow(clippy::too_many_arguments)]
    fn index_set(
        &mut self,
        base: (Place, Type),
        name: &str,
        quoted: &str,
        index: &'a Expr,
        value: &'a Expr,
        sid: NodeId,
        nested: bool,
        whole: Option<Place>,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<(), Gap> {
        self.index_store(&base.1, quoted, index, value, line)?;
        let (base, bty) = base;
        // A name the value can change is read before the value runs.
        let pinned = match self.changed_by(index, value) {
            true => Some(self.bind_temp(&format!("{quoted}[]idx"), index, line, out)?),
            false => None,
        };
        let place = match whole {
            Some(p) => p,
            None if nested && self.projected(&bty) => {
                return gap("a user container below the root of a store", line);
            }
            None => {
                let at = match pinned {
                    Some(at) => at,
                    None if self.is_map(&bty) => self.val(index, out)?,
                    None => self.read_val(index, out)?,
                };
                match self.is_map(&bty) {
                    true => Place::Key(Box::new(base), at),
                    false => Place::Elem(Box::new(base), at),
                }
            }
        };
        // A user container's element type is the value's.
        let ety = match self.elem_ty(&bty, line) {
            Ok(t) => t,
            Err(_) => self.ty_of(value)?,
        };
        // A crossing into a validated element is its constructor, proven or
        // not.
        let mark = out.len();
        let v = match self.checked(&self.ty_of(value)?, &ety, value) {
            Some(to) => Val::Name(self.checked_temp(&to, value, line, out)?),
            None => self.val(value, out)?,
        };
        // A key that owns heap is moved into the map by the store, so it is
        // read after the value; no copy of it exists to read before. Refused
        // where the value changes it, as a scalar index is read first.
        if let (Place::Key(_, Val::Name(k)), Expr::Var { name: key, .. }) = (&place, index) {
            if crate::kernel::modifies(&out[mark..], Root::N(*k), &self.body.names) {
                let refusal = rule!(MapKeyChanged, name, key).render();
                self.body.refused.push((line, refusal));
            }
        }
        let site = Site::Node(sid);
        let releases = self.own.placed.stores.contains_key(&sid);
        out.push(St::Store {
            place,
            value: v,
            old: self.old_for(&ety, releases),
            line,
            site,
            releases,
            holes: if releases {
                (self.own.placed.stores.get(&sid).cloned()).unwrap_or_default()
            } else {
                Vec::new()
            },
        });
        Ok(())
    }

    /// Whether the store's `value` can change `index`, a `mut` name of a scalar
    /// type, before the store reads it: a call is passed the name, or a call
    /// reaches module state.
    fn changed_by(&self, index: &Expr, value: &Expr) -> bool {
        let Expr::Var { name, .. } = index else {
            return false;
        };
        let by: Box<dyn Fn(&[Expr]) -> bool> = match self.lookup(name) {
            Some(n) if self.body.names[n.index()].mutable => Box::new(|args| {
                args.iter()
                    .any(|a| matches!(a, Expr::Var { name: n, .. } if n == name))
            }),
            Some(_) => return false,
            None if self
                .program
                .globals
                .iter()
                .any(|g| g.name == *name && g.mutable) =>
            {
                Box::new(|_| true)
            }
            None => return false,
        };
        vyrn_frontend::ast::calls_with(value, &*by)
            && self.ty_of(index).is_ok_and(|t| !self.owns(&t))
    }

    /// Whether a binding or module state answers `name`. Where none does,
    /// records the refusal `rule` states of the name.
    fn known(&mut self, name: &str, line: usize, rule: fn(&str) -> Rule) -> bool {
        if self.answers(name) {
            return true;
        }
        self.body.refused.push((line, rule(name).render()));
        false
    }

    /// The condition of an `if` or a `while`, read. A condition that is not
    /// Bool is refused; one typed `Err` is refused where its name is.
    fn condition(
        &mut self,
        cond: &'a Expr,
        word: &str,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<Val, Gap> {
        let t = self.ty_of(cond)?;
        let bool = vyrn_frontend::types::resolve(&t, self.proto.types()) == Type::Bool;
        if !bool && t != Type::Err {
            let ([t], []) = self.body.speech().say([&t], []);
            let refusal = rule!(ConditionNotBool, word, t).render();
            self.body.mistyped.push((line, refusal));
        }
        self.read_val(cond, out)
    }

    fn answers(&self, name: &str) -> bool {
        self.lookup(name).is_some()
            || !self.closed && self.program.globals.iter().any(|g| g.name == name)
    }

    /// The refusal of `e` where it names nothing: a variable no binding or
    /// module state answers, or `T?(..)` of an undeclared type.
    fn unknown_of(&self, e: &Expr) -> Option<(usize, String)> {
        match e {
            Expr::Var { name, line, id: _ } if !self.answers(name) && !self.is_variant(name) => {
                Some((*line, rule!(UnknownVariable, name).render()))
            }
            Expr::TryConstruct { name, line, .. } if !self.proto.types().contains_key(name) => {
                Some((*line, rule!(UnknownType, n = name).render()))
            }
            _ => None,
        }
    }

    /// The receiver rule of `pop` and `swapRemove`: the array they shrink is
    /// growable, and its root is a `mut` name. A receiver that is no place, or
    /// whose root is not `mut`, is refused elsewhere (the checker's
    /// `mut_array_receiver`, [`crate::typed::stores`]).
    fn shrinks(&mut self, op: &str, recv: &Expr, line: usize) {
        let Some((_, root, _)) = vyrn_frontend::parser::place_steps(recv) else {
            return;
        };
        let mutable = match self.lookup(root) {
            Some(n) => self.body.names[n.index()].mutable,
            None => self
                .program
                .globals
                .iter()
                .any(|g| g.name == root && g.mutable),
        };
        // A variable is no typed node; a field or an element is.
        let ty = match recv {
            Expr::Var { .. } => self.named_place(root, line).ok().map(|(_, ty)| ty),
            _ => self.ty_of(recv).ok(),
        };
        let Some(ty) = ty.filter(|_| mutable) else {
            return;
        };
        let refusal = match vyrn_frontend::types::resolve(&ty, self.proto.types()) {
            Type::Array(_) | Type::SmallArray(..) | Type::Err => return,
            Type::ArrayN(..) => rule!(ShrinkFixedArray, op),
            other => rule!(
                ShrinkNeedsArray,
                op,
                t = self.body.speech().ty(&other).to_string()
            ),
        };
        self.body.mistyped.push((line, refusal.render()));
    }

    fn unknown_at(&mut self, e: &Expr) {
        self.body.refused.extend(self.unknown_of(e));
    }

    /// A name as a place: a binding of this body, or module state with its
    /// declared type.
    fn named_place(&self, name: &str, line: usize) -> Result<(Place, Type), Gap> {
        if let Some(n) = self.lookup(name) {
            return Ok((Place::Name(n), self.body.names[n.index()].ty.clone()));
        }
        match self.program.globals.iter().find(|g| &g.name == name) {
            Some(g) => match g.ty.clone().or_else(|| node_ty(self.own, g.init.id())) {
                Some(t) => Ok((Place::Global(name.to_string()), t)),
                None => gap_d("a global the checker did not type", name, line),
            },
            None => gap("a place that is not a binding", line),
        }
    }

    /// Binds, once before the loop, the header of every heap container the
    /// loop `l` indexes and no row of it rebuilds ([`crate::kernel::writes`]),
    /// and points the loop's element and length reads at it. The container is
    /// a name, module state, or a field of one. The header is a borrow the
    /// loop walks, so the kernel keeps its alias. A heapless container is a
    /// value, read in place each turn. For module state, a call that stores
    /// into it counts as a write.
    fn hoist_headers(&mut self, l: &mut [St], line: usize, out: &mut Vec<St>) {
        let mut places = Vec::new();
        header_reads(l, None, &mut places);
        // A header an inner loop hoisted is hoisted again here, and the inner
        // borrow reads the outer one's parts.
        l.iter().flat_map(St::rows).for_each(|(s, _)| match s {
            St::Let(h, Rhs::Read(p))
                if self.body.names[h.index()].walked == Some(Walk::While) && fixed(p) =>
            {
                places.push(p.clone())
            }
            _ => {}
        });
        let mut read: Vec<(Root, String, Place)> = (places.into_iter())
            .map(|p| {
                let (r, fields) = crate::kernel::root(&p);
                (r, fields, p)
            })
            .collect();
        read.sort_unstable_by(|(a, pa, _), (b, pb, _)| {
            let by_root = match (a, b) {
                (Root::N(x), Root::N(y)) => x.cmp(y),
                (Root::G(x), Root::G(y)) => x.cmp(y),
                (Root::N(_), Root::G(_)) => std::cmp::Ordering::Less,
                (Root::G(_), Root::N(_)) => std::cmp::Ordering::Greater,
            };
            by_root.then_with(|| pa.cmp(pb))
        });
        read.dedup_by(|a, b| a.2 == b.2);
        let mut bound = Vec::new();
        l.iter().for_each(|s| names_bound(s, &mut bound));
        let decls = self.proto.types();
        'read: for (r, fields, from) in read {
            let (mut ty, mut path, mut heap) = match &r {
                Root::N(n) => {
                    let info = &self.body.names[n.index()];
                    if bound.contains(n) {
                        continue;
                    }
                    let path = info.path.clone().unwrap_or_else(|| info.source.clone());
                    (info.ty.clone(), path, info.heap)
                }
                Root::G(g) => match self.named_place(g, line) {
                    Ok((Place::Global(_), ty)) => {
                        let heap = self.proto.owns_heap(&ty);
                        (ty, self.body.spelled(g).to_string(), heap)
                    }
                    _ => continue,
                },
            };
            for f in fields.split('.').skip(1) {
                let Ok(t) = self.field_ty(&ty, f, line) else {
                    continue 'read;
                };
                heap = self.proto.owns_heap(&t);
                path = format!("{path}.{f}");
                ty = t;
            }
            let indexed = matches!(
                vyrn_frontend::types::resolve(&ty, &decls),
                Type::Array(_) | Type::SmallArray(..) | Type::Str
            );
            if !indexed
                || !heap
                || crate::kernel::writes(
                    l,
                    r,
                    &fields,
                    &self.body.names,
                    self.body.id,
                    &self.own.state_callees,
                )
            {
                continue;
            }
            let h = self.name("@borrow", ty, false, line);
            self.body.names[h.index()].walked = Some(Walk::While);
            self.body.names[h.index()].path = Some(path);
            header_reads(l, Some((&from, h)), &mut Vec::new());
            out.push(St::Let(h, Rhs::Read(from)));
        }
    }

    /// The element place a `for` over the user container `it` reads at the
    /// counter `i`: its `place nth` stated at the site, as
    /// [`Builder::inlined`] states any projection.
    fn for_element(
        &mut self,
        iter: &Expr,
        ity: &Type,
        it: Name,
        i: Name,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<Place, Gap> {
        if !self.program.expansions.is_shared() {
            return Ok(Place::Elem(Box::new(Place::Name(it)), Val::Name(i)));
        }
        let p = match self
            .program
            .expansions
            .for_element(&self.program.impls, ity, iter, line)
        {
            Ok(Some(p)) => p,
            Ok(None) => return gap("a `for` over a container with no `nth`", line),
            Err(e) => return gap_d("a projection this site cannot inline", &e, line),
        };
        let mark = self.frame.scope.len();
        self.frame
            .scope
            .push((vyrn_frontend::project::FOR_RECV.to_string(), it));
        self.frame
            .scope
            .push((vyrn_frontend::project::FOR_INDEX.to_string(), i));
        let r = p
            .prologue
            .iter()
            .try_for_each(|s| self.stmt(s, out))
            .and_then(|()| self.place(&p.place, out));
        self.frame.scope.truncate(mark);
        r
    }

    /// The length a `for` over `it` walks to: a header read for a built-in
    /// container, the `Iterate` impl's `size` for a user one.
    fn length_of(&self, it: Name, ity: &Type, line: usize) -> Result<Rhs, Gap> {
        let decls = self.proto.types();
        let field = |f: &str| Rhs::Read(Place::Field(Box::new(Place::Name(it)), f.to_string()));
        Ok(match vyrn_frontend::types::resolve(ity, &decls) {
            Type::Str => field("byteLength"),
            t if t.is_seq() || matches!(t, Type::Map(..)) => field("length"),
            _ => match vyrn_frontend::types::iterate_impl(&self.program.impls, ity) {
                Some((imp, size, _)) => {
                    let solved = self.impl_args(imp, &size, ity);
                    Rhs::Call {
                        kind: (self.fn_id(&size).filter(|_| solved.is_some()))
                            .map_or(Callee::Method, Callee::Fn),
                        callee: size,
                        args: vec![(Arg::Val(Val::Name(it)), Capability::Read)],
                        write_back: false,
                        ret: Some(Type::Int),
                        solved: solved.unwrap_or_default(),
                        targets: Vec::new(),
                    }
                }
                None => return gap("a `for` over a container with no length", line),
            },
        })
    }

    /// `i = i + 1`, for the counter of a `for` over an index.
    fn step(&mut self, i: Name, out: &mut Vec<St>) {
        let line = self.body.names[i.index()].line;
        let t = self.temp(Type::Int, line);
        out.push(St::Let(
            t,
            Rhs::Prim(
                Op::Bin(BinOp::Add),
                vec![Val::Name(i), Val::Lit(Lit::Int(1))],
                Some(Type::Int),
            ),
        ));
        out.push(St::Store {
            place: Place::Name(i),
            value: Val::Name(t),
            old: Old::Nothing,
            line,
            site: Site::None,
            releases: false,
            holes: Vec::new(),
        });
    }

    fn projected_elem(&self, ity: &Type) -> Option<Type> {
        let (imp, nth) = self.program.impls.place(ity, "nth")?;
        Some(vyrn_frontend::types::under_head(imp, ity, &nth.ret))
    }

    /// Whether a projection answers for `ty`'s element place.
    fn projected(&self, ty: &Type) -> bool {
        self.program.impls.place(ty, "atSet").is_some()
    }

    /// The type arguments of a call to `f`, a function `imp` flattened, on a
    /// receiver of type `recv`, in `f`'s order: the impl head's parameters
    /// solved against the receiver. Empty for a function with none; `None`
    /// where the program declares no `f` or the receiver leaves a parameter
    /// unsolved.
    fn impl_args(
        &self,
        imp: &vyrn_frontend::ast::ImplBlock,
        f: &str,
        recv: &Type,
    ) -> Option<Vec<(String, Type)>> {
        let g = self.program.functions.iter().find(|g| g.name == f)?;
        let mut subst = HashMap::new();
        vyrn_frontend::types::solve_param(&imp.ty, recv, &mut subst);
        (g.type_params.iter())
            .map(|p| Some((p.clone(), subst.get(p)?.clone())))
            .collect()
    }

    /// The id of the function the program declares under `name`.
    fn fn_id(&self, name: &str) -> Option<FnId> {
        (self.program.functions.iter())
            .position(|f| f.name == name)
            .map(FnId::nth)
    }

    fn is_map(&self, ty: &Type) -> bool {
        let decls = self.proto.types();
        matches!(vyrn_frontend::types::resolve(ty, &decls), Type::Map(..))
    }

    fn field_ty(&self, ty: &Type, field: &str, line: usize) -> Result<Type, Gap> {
        let decls = self.proto.types();
        let rt = vyrn_frontend::types::resolve(ty, &decls);
        match rt {
            Type::Record(fields) => fields
                .iter()
                .find(|f| f.name == field)
                .map(|f| f.ty.clone())
                .ok_or(Gap {
                    what: "a field the record does not have",
                    detail: field.to_string(),
                    line,
                    rule: None,
                }),
            _ => gap("a field of a non-record", line),
        }
    }

    fn elem_ty(&self, ty: &Type, line: usize) -> Result<Type, Gap> {
        let decls = self.proto.types();
        let t = vyrn_frontend::types::resolve(ty, &decls);
        match (t.elem(), &t) {
            (Some(e), _) => Ok(e.clone()),
            (None, Type::Stream(e)) => Ok((**e).clone()),
            // A `for` over a String yields each byte as an `Int64`, the
            // checker's type; `s[i]` is a `UInt8` and never reaches here.
            (None, Type::Str) => Ok(Type::Int),
            (None, Type::Map(_, v)) => Ok((**v).clone()),
            (None, t) => gap_d("an element of a non-container", &t.to_string(), line),
        }
    }

    /// Whether a construct owns the boxes its binders come out of
    /// ([`St::Switch`]'s `owns`): it took a named scrutinee, or it switches
    /// on a value the frame made. A declared release's receiver is excepted:
    /// its caller frees the payload boxes after the call.
    fn owns_boxes(&self, e: &'a Expr, consuming: bool) -> bool {
        let receiver = match e {
            Expr::Consume { place, .. } => match &**place {
                Expr::Var { name, .. } => Some(name),
                _ => None,
            },
            Expr::Var { name, .. } => Some(name),
            _ => None,
        };
        let released = receiver.is_some_and(|r| {
            self.frame.released.is_some() && self.lookup(r) == self.frame.released
        });
        !released && (consuming || self.made_scrutinee(e))
    }

    /// Whether the frame made the value a construct switches on: anything but
    /// a name or a place. `m[k]` on a `Map` is a place: its `Option<V>`
    /// borrows the entry, so its binders borrow too (#463).
    fn made_scrutinee(&self, e: &'a Expr) -> bool {
        use vyrn_frontend::ast::place_path;
        use vyrn_frontend::project::element_path;
        place_path(e).is_none() && element_path(e, &self.own.place_names).is_none()
    }

    /// The scrutinee of a `match`, `if let` or `?`: the value it switches on,
    /// and whether the construct consumed it. `lines` is the construct's first
    /// and last line ([`Builder::takes_scrutinee`]); `None` for an `if let`
    /// or a `?`, which read a named local.
    fn scrutinee(
        &mut self,
        e: &'a Expr,
        construct: NodeId,
        lines: Option<(usize, usize)>,
        out: &mut Vec<St>,
    ) -> Result<(Val, bool), Gap> {
        if !matches!(e, Expr::Var { .. }) && is_place_read(e) {
            // A field or an element: the construct borrows it, and its
            // binders borrow what they name.
            let v = self.read_val(e, out)?;
            return Ok((v, false));
        }
        match e {
            Expr::Var { name, .. } if self.lookup(name).is_some() => {
                let n = self.lookup(name).unwrap();
                if !self.takes_scrutinee(n, lines) {
                    return Ok((Val::Name(n), false));
                }
                // Recorded as a candidate; only a site [`last_owner`] seeded
                // takes it.
                self.body.cands.push((construct, n, Cand::Switch));
                if !self.seed.contains(&construct) {
                    return Ok((Val::Name(n), false));
                }
                let t = self.temp(self.body.names[n.index()].ty.clone(), e.line());
                out.push(St::Let(t, Rhs::Val(Val::Name(n))));
                self.keyed(t, construct);
                Ok((Val::Name(t), true))
            }
            Expr::Consume { place, line, id } => match &**place {
                Expr::Var { name, .. } => {
                    let Some(n) = self.lookup(name) else {
                        // `consume` of module state: the same read and refusal.
                        let v = self.global_read(place, name, *line, out)?;
                        if let Val::Name(t) = v {
                            self.keyed(t, construct);
                        }
                        return Ok((v, false));
                    };
                    let t = self.temp(self.body.names[n.index()].ty.clone(), *line);
                    out.push(St::Let(t, Rhs::Val(Val::Name(n))));
                    self.keyed(t, construct);
                    Ok((Val::Name(t), self.taken_by(t, construct)))
                }
                _ => {
                    let Val::Name(t) = self.take_prefix(place, id.col(), *line, out)? else {
                        return gap("a `consume` of a literal", *line);
                    };
                    self.keyed(t, construct);
                    Ok((Val::Name(t), self.taken_by(t, construct)))
                }
            },
            _ => {
                let outer = self.frame.scrutinee.replace(e.id());
                let v = self.val(e, out);
                self.frame.scrutinee = outer;
                match v? {
                    Val::Name(t) => {
                        self.keyed(t, construct);
                        Ok((Val::Name(t), self.taken_by(t, construct)))
                    }
                    Val::Lit(l) => Ok((Val::Lit(l), false)),
                }
            }
        }
    }

    /// Whether this construct is a candidate to take its named scrutinee:
    /// every owned, heap-owning named scrutinee of a `match` is, and
    /// [`last_owner`] decides which construct is its last owner. `lines` is
    /// `None` for an `if let` or a `?`, which cannot hand a payload out.
    ///
    /// Too wide an answer is a refusal, never a double free: the take is
    /// stated in the core, so the kernel refuses a later read.
    fn takes_scrutinee(&self, n: Name, lines: Option<(usize, usize)>) -> bool {
        let info = &self.body.names[n.index()];
        lines.is_some()
            && info.releases
            && info.heap
            && !info.borrow
            && !self.frame.reading.contains(&n)
    }

    /// Whether the construct took the temporary `t` it owns: the payloads
    /// moved into the arms' binders and the boxes were freed there. Where it
    /// did not, the binders borrowed and the value is released whole. Records
    /// the candidate; only a site [`last_owner`] seeded takes.
    fn taken_by(&mut self, t: Name, construct: NodeId) -> bool {
        if !self.body.names[t.index()].releases {
            return false;
        }
        self.body.cands.push((construct, t, Cand::Switch));
        self.seed.contains(&construct)
    }

    /// [`Builder::taken_by`] at a `for`: whether the loop is the last owner of
    /// its container, so it releases it where it ends ([`Cand::Loop`]).
    fn taken_by_loop(&mut self, it: Name, sid: NodeId) -> bool {
        self.body.cands.push((sid, it, Cand::Loop));
        self.seed.contains(&sid)
    }

    /// Which tag a pattern tests for, in the scrutinee's variant list. `??`'s
    /// pair names a tag: 1 succeeds and 0 fails, for every sum.
    fn arm_test(&self, p: &Pattern, rt: &Type, line: usize) -> Result<Test, Gap> {
        let Pattern::Variant(v, _) = p else {
            return Ok(match p {
                Pattern::Other => Test::Else,
                _ => Test::Tag(u64::from(matches!(p, Pattern::Success(_)))),
            });
        };
        let Type::Enum(variants) = rt else {
            return gap("a variant pattern on a non-enum", line);
        };
        match variants.iter().position(|x| x.name == *v) {
            Some(at) => Ok(Test::Tag(at as u64)),
            None => gap("a variant the enum does not have", line),
        }
    }

    /// Refuses a `match` whose arms do not take each variant of `sty` once:
    /// a tag twice, or, with no default arm, a variant no arm takes. A
    /// `match` with no arm also gets a `trap` ahead of the switch, so the
    /// kernel never joins a switch no edge leaves.
    fn covers(
        &mut self,
        arms: &[Arm],
        rt: &Type,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<(), Gap> {
        let Type::Enum(variants) = rt else {
            return gap("a variant pattern on a non-enum", line);
        };
        if arms.is_empty() {
            out.push(St::Trap);
        }
        let mut taken = vec![false; variants.len()];
        for a in arms {
            if let Test::Tag(t) = a.test {
                let Some(seen) = taken.get_mut(t as usize) else {
                    return gap("a variant the enum does not have", line);
                };
                if *seen {
                    let v = self.body.speech().name(&variants[t as usize].name);
                    self.body
                        .refused
                        .push((line, rule!(DuplicateArm, v).render()));
                    return Ok(());
                }
                *seen = true;
            }
        }
        if arms.iter().any(|a| a.test == Test::Else) {
            return Ok(());
        }
        if let Some((v, _)) = variants.iter().zip(&taken).find(|(_, t)| !**t) {
            let v = self.body.speech().name(&v.name);
            let refusal = rule!(MissingVariant, v).render();
            self.body.refused.push((line, refusal));
        }
        Ok(())
    }

    /// Binds a pattern's names: owned binders when the match consumed its
    /// scrutinee, borrowed places otherwise. `from` is the scrutinee's name
    /// where the construct did not consume it: a binder over a borrow carries
    /// the borrow's kind, so `match o { Some(v) => take(v) }` over a `read`
    /// parameter is refused.
    #[allow(clippy::too_many_arguments)]
    fn bind_pattern(
        &mut self,
        p: &Pattern,
        sty: &Type,
        rt: &Type,
        consuming: bool,
        line: usize,
        from: Option<Name>,
        out: &mut Vec<St>,
    ) -> Result<Vec<Name>, Gap> {
        let decls = self.proto.types();
        let (payloads, variant): (Vec<(String, Type, NodeId)>, String) = match p {
            Pattern::Other => (Vec::new(), String::new()),
            // `??`'s pair names a tag: variant 1 succeeds, 0 fails.
            Pattern::Success(n) | Pattern::Failure(n) => match rt {
                Type::Enum(vs) if vs.len() == 2 => {
                    let at = usize::from(matches!(p, Pattern::Success(_)));
                    let ps = vs[at]
                        .payload
                        .first()
                        .map(|t| vec![(n.name.clone(), t.clone(), n.id())])
                        .unwrap_or_default();
                    (ps, vs[at].name.clone())
                }
                _ => return gap("a `??` pattern on a scrutinee with no two tags", line),
            },
            Pattern::Variant(v, names) => match rt {
                Type::Enum(variants) => {
                    let Some(var) = variants.iter().find(|x| x.name == *v) else {
                        return gap("a variant the enum does not have", line);
                    };
                    if var.payload.len() != names.len() {
                        return gap("a variant pattern with the wrong arity", line);
                    }
                    let ps = names
                        .iter()
                        .zip(var.payload.iter().cloned())
                        .map(|(n, t)| (n.name.clone(), t, n.id()))
                        .collect();
                    (ps, var.name.clone())
                }
                _ => return gap("a variant pattern on a non-enum", line),
            },
        };
        let mut binds = Vec::new();
        for (i, (name, ty, key)) in payloads.into_iter().enumerate() {
            let owned = consuming && self.owns(&ty);
            let layout = matches!(
                vyrn_frontend::types::resolve(&ty, &decls),
                Type::Record(_)
                    | Type::Enum(_)
                    | Type::Array(_)
                    | Type::ArrayN(..)
                    | Type::SmallArray(..)
                    | Type::Map(..)
            );
            let n = self.name(&name, ty, owned, line);
            // The placer keys a binder's exit rows (a `return`, `break` or
            // `continue` inside the arm) by this address. Keyed owned or not:
            // the first build takes nothing, so the second build's rows need
            // a name to land on; [`Builder::drops_at`] skips a borrowed one.
            self.keyed(n, key);
            if !owned {
                if let Some(m) = from {
                    if let Some(k) = self.body.names[m.index()].borrow_kind.clone() {
                        self.body.names[n.index()].borrow_kind = Some(k);
                        self.body.names[n.index()].must_use_param =
                            self.body.names[m.index()].must_use_param;
                    }
                    // A scrutinee that is a place read has no kind to pass
                    // on, so the binder is `Place` (`return match d.tag {
                    // Word(s) => s, .. }`). An owned scrutinee's payloads are
                    // the frame's to give.
                    if self.body.names[m.index()].borrow
                        && self.body.names[n.index()].borrow_kind.is_none()
                    {
                        self.body.names[n.index()].borrow_kind = Some(BorrowKind::Place);
                    }
                    // A binder of a layout or a heap value reads the scrutinee
                    // for the arm's extent ([`Arm::reads`]), so the kernel
                    // refuses a write to it meanwhile.
                    if layout || self.body.names[n.index()].borrow {
                        // The walk skips a payload hole on its live tag, and
                        // only a declared `release` cannot skip one
                        // ([`vyrn_frontend::declared::skippable`]).
                        let at = format!("{variant}.{i}");
                        self.body.names[n.index()].payload = Some(
                            if vyrn_frontend::declared::skippable(
                                &self.own.proto,
                                sty,
                                std::slice::from_ref(&at),
                            ) {
                                Payload::Hole(format!(".{at}"))
                            } else {
                                Payload::Sealed(sty.to_string())
                            },
                        );
                        out.push(St::Let(n, Rhs::Read(Place::Name(m))));
                    }
                }
            }
            // `_` never enters the scope, but its payload is real and a
            // consumed scrutinee's arm still owes its release.
            if name != "_" {
                self.frame.scope.push((name, n));
            }
            binds.push(n);
        }
        Ok(binds)
    }

    /// An expression in a read position: an operand, a condition, an index, a
    /// `read` argument. A heap place is borrowed, not moved. A String
    /// temporary (`@str`, `@concat`, a string `+`) is freed by the reading
    /// site, so its drop is queued after the consuming binding.
    fn read_val(&mut self, e: &'a Expr, out: &mut Vec<St>) -> Result<Val, Gap> {
        self.read_at(e, out, None)
    }

    /// A read in an argument position, with the position it fills. An
    /// operator is a call (`a + b` is `@concat(a, b)`), so its operands come
    /// through here too.
    fn read_arg(
        &mut self,
        e: &'a Expr,
        out: &mut Vec<St>,
        callee: &str,
        ix: usize,
    ) -> Result<Val, Gap> {
        let of = self.own.arg_caps.named(callee);
        self.read_at(e, out, Some((callee, of, ix)))
    }

    fn read_at(
        &mut self,
        e: &'a Expr,
        out: &mut Vec<St>,
        at: Option<(&str, CapsOf, usize)>,
    ) -> Result<Val, Gap> {
        let v = self.read_val_inner(e, out)?;
        // The argument-drop key, here rather than in `call`, which sees
        // neither an operator nor a `lazy` field read.
        if let (Val::Name(t), Some((callee, of, ix))) = (&v, at) {
            let t = *t;
            if self.arg_released(e, t, callee, of, ix) {
                self.body.names[t.index()].arg_drop = Some(e.id());
            }
        }
        Ok(v)
    }

    /// Whether a store whose value mentions the place it writes into hands
    /// nothing back, because every mention is a read the value cannot hand
    /// back. A function whose result is not spelled `read`/`modify` returns
    /// an owned value; the kernel refuses returning a borrow of a parameter.
    fn store_is_fresh(&self, value: &'a Expr, name: &str) -> bool {
        // A heapless value has nothing to hand back.
        if !self.ty_of(value).is_ok_and(|t| self.proto.owns_heap(&t)) {
            return false;
        }
        let mut ms = Vec::new();
        self.read_only_mentions(value, name, &mut ms)
    }

    /// Whether every mention of `root` in `e` is a read that cannot hand
    /// `root`'s own storage back.
    fn read_only_mentions(&self, e: &Expr, root: &str, out: &mut Vec<String>) -> bool {
        use vyrn_frontend::movecheck as mc;
        if !vyrn_frontend::ast::mentions_place(e, root) {
            return true;
        }
        // A heapless mention hands nothing back (`Frag { start: f.start }`).
        if self.ty_of(e).is_ok_and(|t| !self.proto.owns_heap(&t)) {
            return true;
        }
        match e {
            // A call that forwards none of its arguments builds its result
            // afresh (`xs[j].copy()`), whatever it reads.
            Expr::Call { name, .. } if !mc::call_may_forward(name, &self.own.place_names) => true,
            Expr::Call { name, args, .. } => args.iter().all(|a| {
                let is_root_read = match a {
                    Expr::Var { name: v, .. } => v == root,
                    _ => vyrn_frontend::ast::place_path(a).is_some_and(|(r, _)| r == root),
                };
                if !is_root_read {
                    return self.read_only_mentions(a, root, out);
                }
                if !mc::call_may_forward(name, &self.own.place_names) {
                    true
                } else if self.declares(name)
                    && !name.starts_with('@')
                    && prelude::signature(name).is_none()
                {
                    out.push(name.clone());
                    true
                } else {
                    false
                }
            }),
            Expr::Binary { lhs, rhs, .. } => {
                self.read_only_mentions(lhs, root, out) && self.read_only_mentions(rhs, root, out)
            }
            Expr::Unary { expr, .. } => self.read_only_mentions(expr, root, out),
            Expr::StructLit { fields, .. } => fields
                .iter()
                .all(|(_, v)| self.read_only_mentions(v, root, out)),
            Expr::ArrayLit { elems, .. } => {
                elems.iter().all(|v| self.read_only_mentions(v, root, out))
            }
            _ => false,
        }
    }

    /// Whether the program declares a callable of this name: a function, a
    /// method, a projection, or a seeded builtin.
    fn declares(&self, name: &str) -> bool {
        self.program.functions.iter().any(|f| f.name == name)
            || self
                .program
                .impls
                .iter()
                .any(|i| i.methods.iter().any(|m| m.name == name))
            || self.projection(name).is_some()
            || prelude::signature(name).is_some()
    }

    /// Whether the temporary this frame minted for an argument position is
    /// the caller's to release after the call. It must be an allocation
    /// ([`NameInfo::releases`], or a forced `lazy` field), and
    /// [`vyrn_frontend::movecheck::arg_verdict`] decides what the callee does
    /// with it.
    fn arg_released(&self, e: &'a Expr, t: Name, callee: &str, of: CapsOf, ix: usize) -> bool {
        use vyrn_frontend::movecheck as mc;
        // A named value is nobody's temporary: `f(s)` hands over what `s`
        // owns, and the binding keeps the row.
        if matches!(e, Expr::Var { .. } | Expr::Consume { .. }) {
            return false;
        }
        // A forced `lazy` field read is a call returning a fresh owned value.
        // This pass binds a borrow for it because the read names
        // a place, but the caller still frees the value.
        let info = &self.body.names[t.index()];
        let forced = self.forces_a_thunk(e);
        if !info.releases && !forced {
            return false;
        }
        // `blackBox` of a borrow built nothing to free; of an owned
        // temporary, it took it, and its result frees it.
        if let Expr::Call { name, args, .. } = e {
            if self.hands_back_a_borrow(name, args) {
                return false;
            }
        }
        let ty = if forced {
            match self.forced_ty(e) {
                Some(t) => t,
                None => return false,
            }
        } else {
            info.ty.clone()
        };
        if self.proto.release_kind(&ty).is_none() {
            return false;
        }
        // The producer as `arg_verdict` partitions it: a call's name, `None`
        // for the allocating operator, else a name no user function can have.
        let producer = match e {
            Expr::Call { name, .. } => Some(name.clone()),
            Expr::Binary { op: BinOp::Add, .. } => None,
            Expr::Match { .. } => Some("@match".to_string()),
            Expr::StructLit { .. } => Some("@record".to_string()),
            Expr::ArrayLit { .. } => Some("@heapify".to_string()),
            Expr::Field { .. } => Some("@lazy".to_string()),
            _ => Some("@build".to_string()),
        };
        // A view LENDS, unless the element it hands out is a heap-free copy.
        let decls = self.proto.types();
        let views = mc::lends_result(callee, &self.own.place_names);
        let view_copies = views
            && matches!(
                vyrn_frontend::types::resolve(&ty, &decls),
                Type::Array(ref et)
                    | Type::ArrayN(ref et, _)
                    | Type::SmallArray(ref et, _)
                    | Type::Stream(ref et) if !self.proto.owns_heap(et)
            );
        let s = mc::ArgTemp {
            callee: callee.to_string(),
            ix,
            producer,
            views,
            view_copies,
            constructs: matches!(callee, "Some" | "Ok" | "Err" | "Success" | "Failure")
                || self.is_variant(callee),
            cap: self.own.arg_caps.at(of, ix),
        };
        if mc::arg_verdict(&s) == mc::ArgVerdict::Released {
            return true;
        }
        // A call through a fn value has no capability row, but every target
        // reads every argument and keeps nothing: a named function by
        // `Checker::reads_every_param`, a lambda by its frame's `read`
        // parameters. As in `examples/rpc.vyrn`'s `cb(Done(..))`.
        self.lookup(callee).is_some_and(|n| {
            let ty = &self.body.names[n.index()].ty;
            matches!(
                vyrn_frontend::types::resolve(ty, &self.proto.types()),
                Type::Fn(..)
            )
        })
    }

    /// The type a forced `lazy` field read yields, where it owns heap.
    fn forced_ty(&self, e: &Expr) -> Option<Type> {
        self.deferred_of(e).filter(|t| self.proto.owns_heap(t))
    }

    /// The `T` of a read of a `lazy T` field.
    fn deferred_of(&self, e: &Expr) -> Option<Type> {
        let Expr::Field {
            expr: base, field, ..
        } = e
        else {
            return None;
        };
        let decls = self.proto.types();
        let bt = self.ty_of(base).ok()?;
        let Type::Record(fields) = &*vyrn_frontend::types::resolved(&bt, &decls) else {
            return None;
        };
        let f = fields.iter().find(|f| &f.name == field)?;
        vyrn_frontend::types::deferred(&f.ty).cloned()
    }

    /// Whether reading `e` forces a `lazy` field at some step. Such a read
    /// names a part of a fresh value, not a place, though
    /// [`is_place_read`] answers by its spelling.
    fn forces(&self, e: &Expr) -> bool {
        match e {
            Expr::Field { expr, .. } => self.deferred_of(e).is_some() || self.forces(expr),
            Expr::Call { args, .. } if reads_a_part(e) => self.forces(&args[0]),
            _ => false,
        }
    }

    /// Forces a read of a `lazy T` field: borrows the stored
    /// nullary closure out of the field and calls through it. The result is
    /// fresh on every read; its release is keyed by the read (`arg_released`).
    fn force(&mut self, e: &'a Expr, inner: Type, out: &mut Vec<St>) -> Result<Rhs, Gap> {
        let Expr::Field { expr, field, .. } = e else {
            return gap("a forced read of no field", e.line());
        };
        let place = Place::Field(Box::new(self.place(expr, out)?), field.clone());
        let thunk = Type::Fn(Vec::new(), Box::new(inner.clone()));
        let n = self.name("@thunk", thunk, false, e.line());
        let callee = format!("@thunk{}", n.0);
        self.body.names[n.index()].source = callee.clone();
        self.body.names[n.index()].path = self.reader_path(e);
        out.push(St::Let(n, Rhs::Read(place)));
        // The thunk borrows the receiver until the call has run, and the
        // result owns nothing of it: the receiver goes once the result is
        // bound.
        if let Some((r, ..)) = self.frame.pending_receiver.take() {
            self.frame.after.push(r);
        }
        Ok(Rhs::Call {
            callee,
            args: Vec::new(),
            write_back: false,
            kind: Callee::Value(n),
            ret: Some(inner),
            solved: Vec::new(),
            targets: Vec::new(),
        })
    }

    fn forces_a_thunk(&self, e: &Expr) -> bool {
        self.forced_ty(e).is_some()
    }

    fn read_val_inner(&mut self, e: &'a Expr, out: &mut Vec<St>) -> Result<Val, Gap> {
        let ty = self.ty_of(e).ok();
        let owns = ty.as_ref().is_some_and(|t| self.owns(t));
        let is_place = match e {
            Expr::Field { .. } => self.deferred_of(e).is_none(),
            Expr::Call { name, args, .. } => name == "@at" && args.len() == 2,
            _ => false,
        };
        match e {
            _ if owns && is_place => {
                let place = self.place(e, out)?;
                let t = self.borrow_name(e, ty.unwrap(), e.line());
                out.push(St::Let(t, Rhs::Read(place)));
                self.release_receiver(e, out, true);
                Ok(Val::Name(t))
            }
            Expr::Var { .. } | Expr::Consume { .. } => self.val(e, out),
            Expr::Lambda { .. } => self.lambda(e, out),
            _ if owns => {
                let v = self.val(e, out)?;
                if let Val::Name(t) = v {
                    if self.body.names[t.index()].releases && !self.frame.after.contains(&t) {
                        self.frame.after.push(t);
                    }
                }
                Ok(v)
            }
            _ => self.val(e, out),
        }
    }

    /// Frees the unnamed receiver of a field or element read after the read,
    /// where this frame owns it: a receiver is pending only where
    /// [`Builder::place`] minted an owned name for it. The hole is the field
    /// the read took; a scalar read leaves none.
    ///
    /// `borrowed` says the consumer borrows a heap value out of the receiver
    /// (`f(x).rhs.startsWith("{")`), so the receiver must outlive the
    /// consumer: its free is an argument-temporary drop keyed by the
    /// producing node, which the backends free after the consuming call or
    /// operator. Outside such a drain the receiver stays held and the
    /// judgment refuses it.
    fn release_receiver(&mut self, e: &'a Expr, out: &mut Vec<St>, borrowed: bool) {
        let Some((r, producer, malloc)) = self.frame.pending_receiver.take() else {
            return;
        };
        let took = self.ty_of(e).is_ok_and(|t| self.owns(&t));
        if borrowed && took {
            if self.own.placed.producers.contains(&producer) {
                if !self.frame.after.contains(&r) {
                    self.body.names[r.index()].arg_drop = Some(producer);
                    self.frame.after.push(r);
                }
            } else if self.frame.drain > 0 {
                self.body.names[r.index()].producer = Some(producer);
            }
            return;
        }
        // An element's receiver is `@at`'s argument, and its release is keyed
        // as an argument temporary's.
        if let (false, Expr::Call { name, args, .. }) = (took, e) {
            if self.arg_released(&args[0], r, name, self.own.arg_caps.named(name), 0) {
                self.body.names[r.index()].arg_drop = Some(producer);
            }
        }
        // A heap value taken out leaves a hole the release walks around. An
        // element cannot be skipped, so its receiver stays held and the
        // kernel reports it.
        let holes: Vec<String> = match (took, e) {
            (false, _) => Vec::new(),
            (true, Expr::Field { .. }) => match taken_path(e, &|x| self.deferred_of(x).is_some()) {
                Some(path) => vec![path],
                None => return,
            },
            (true, _) => return,
        };
        self.drop_receiver(r, malloc, holes, out);
    }

    /// Releases the unnamed receiver `r` of a part read, around `holes`.
    fn drop_receiver(&mut self, r: Name, malloc: bool, holes: Vec<String>, out: &mut Vec<St>) {
        self.body.names[r.index()].holes = holes;
        self.body.names[r.index()].receiver_malloc = malloc;
        out.push(St::Drop(r, Site::None, 0, None));
    }

    /// Records [`NameInfo::fields`] on the name a record literal is bound to.
    /// Called at both binding sites: a reader's `let` and an inline
    /// literal's temporary.
    fn record_fields(&mut self, n: Name, value: &Expr) {
        if let Expr::StructLit {
            name: t, fields, ..
        } = value
        {
            let t = self.body.spelled(t);
            let fields = fields.iter().map(|(f, _)| format!("{t}.{f}"));
            self.body.names[n.index()].fields = fields.collect();
        }
    }

    /// An expression in a take position: a `let`, a `return`, a store, a part
    /// of a literal, a `consume` argument.
    fn val(&mut self, e: &'a Expr, out: &mut Vec<St>) -> Result<Val, Gap> {
        // The rebind flag covers this expression only: in
        // `n = n + size(if c { names } else { .. })` the join still owns.
        let rebinding = std::mem::take(&mut self.frame.rebinding);
        if let Some(l) = lit_of(e).or_else(|| self.schema(e)) {
            return Ok(Val::Lit(l));
        }
        match e {
            Expr::Var { name, line, id: _ } => match self.lookup(name) {
                Some(n) => {
                    self.spell_take(e, n);
                    Ok(Val::Name(n))
                }
                // A nullary constructor (`None`, a fieldless variant) parses
                // as a bare name. Like a literal, it owns and borrows nothing.
                None if self.is_nullary(name) => {
                    let ty = self.ty_of(e)?;
                    self.nullary(name, ty, *line, out)
                }
                // A function's name stored as a value: the closure enum's
                // variant for it, which captures and owns nothing.
                None if self
                    .program
                    .functions
                    .iter()
                    .any(|f| &f.name == name && f.type_params.is_empty())
                    && self.types.get(&e.id()).is_some_and(|t| {
                        matches!(
                            vyrn_frontend::types::resolve(t, self.proto.types()),
                            Type::Fn(..)
                        )
                    }) =>
                {
                    let ty = self.types[&e.id()].clone();
                    let f = self.program.functions.iter().find(|f| &f.name == name);
                    let decls = self.proto.types();
                    let refusal = stored_slot(None, f, &ty, &decls, *line, &self.body.speech());
                    self.body.mistyped.extend(refusal);
                    let t = self.name("@closure", ty, false, *line);
                    self.body.names[t.index()].borrow = false;
                    self.body.names[t.index()].not_owned = Some(NotOwned::Static);
                    let made = Ctor::Closure(Target::Fn(name.clone()));
                    out.push(St::Let(t, Rhs::Make(made, Vec::new())));
                    Ok(Val::Name(t))
                }
                // A function's name as a value (`sortWith(es, byCount)`), or
                // a type's as an argument (`fromJson(Bag, src)`): static, and
                // the checker types neither as an expression.
                None if self.program.functions.iter().any(|f| &f.name == name)
                    || self.program.contracts.iter().any(|c| &c.name == name)
                    || self.proto.types().contains_key(name) =>
                {
                    Ok(Val::Lit(Lit::Opaque(Opaque::Static)))
                }
                // Nothing may take module state, so a read of it
                // is a borrow in any position.
                None => self.global_read(e, name, *line, out),
            },
            Expr::Consume { place, line, id } => match &**place {
                Expr::Var { name, .. } => match self.lookup(name) {
                    Some(n) => Ok(Val::Name(n)),
                    // `consume <module state>`: a borrow whose take the
                    // kernel refuses.
                    None => self.global_read(place, name, *line, out),
                },
                _ => self.take_prefix(place, id.col(), *line, out),
            },
            Expr::Lambda { .. } => self.lambda(e, out),
            _ => {
                let ty = self.ty_of(e)?;
                if is_place_read(e) && self.owns(&ty) && !self.forces(e) {
                    // `best = m.name`: a borrow (`movecheck::names_a_place`),
                    // so a take of it needs a `.copy()`.
                    let place = self.place(e, out)?;
                    let t = self.borrow_name(e, ty, e.line());
                    out.push(St::Let(t, Rhs::Read(place)));
                    self.release_receiver(e, out, true);
                    return Ok(Val::Name(t));
                }
                let rhs = self.rhs(e, out)?;
                // A `panic` leaves no value to name: every row that reads
                // this one follows the `trap`, and [`cut`] drops it.
                if let Rhs::Val(v @ Val::Lit(Lit::Opaque(Opaque::Trapped))) = rhs {
                    return Ok(v);
                }
                // An `if` or `match` expression whose arm yields a borrow
                // yields a borrow (`movecheck::names_a_place`).
                let borrows = matches!(&rhs, Rhs::Val(v) if self.borrows(v));
                let t = if self.lends(e) || borrows {
                    self.borrow_name(e, ty, e.line())
                } else {
                    // `size(if c { names } else { [..] })`: the temporary owns
                    // the result as a `let` would ([`Builder::loop_alias`]).
                    if !rebinding && self.owns(&ty) {
                        self.loop_alias(&rhs, e.line())?;
                    }
                    self.temp(ty, e.line())
                };
                self.record_fields(t, e);
                self.bind(t, rhs, out);
                self.hold(t);
                if reads_a_part(e) {
                    self.release_receiver(e, out, false);
                }
                Ok(Val::Name(t))
            }
        }
    }

    /// The nullary constructor `name` of `ty`, bound to a temporary.
    fn nullary(
        &mut self,
        name: &str,
        ty: Type,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<Val, Gap> {
        let rhs = self.call(name, &[], line, Some(ty.clone()), None, out)?;
        let t = self.name("@nullary", ty, false, line);
        self.body.names[t.index()].borrow = false;
        self.body.names[t.index()].not_owned = Some(NotOwned::Static);
        out.push(St::Let(t, rhs));
        Ok(Val::Name(t))
    }

    /// A lambda literal as a closure value. Captures are reads of
    /// the enclosing names, which the enclosing frame still owns; a stored
    /// closure snapshots them and owns its snapshot. A literal a
    /// call's target names is monomorphized away ([`Builder::targets_of`]).
    /// The body is its own frame ([`Builder::lambda_frame`]).
    fn lambda(&mut self, e: &'a Expr, out: &mut Vec<St>) -> Result<Val, Gap> {
        let caps = self.captures(e);
        let ty = self.ty_of(e).unwrap_or(Type::Unit);
        let t = self.name("@lambda", ty.clone(), self.owns(&ty), e.line());
        // Taken from the cell, so a nested or sibling lambda asks its own
        // position ([`NameInfo::closure_reads`]).
        self.body.names[t.index()].closure_reads = self.closure_reads(e, &caps);
        let key = self.lambda_frame(e, &caps)?;
        out.push(St::Let(t, Rhs::Prim(Op::Closure(key), caps, Some(ty))));
        Ok(Val::Name(t))
    }

    /// Builds the lambda's own frame, judged like a function's, and answers
    /// its key. Captures are borrowed inputs and parameters are `read`.
    /// The plan keys its bindings' rows by the lambda's nodes
    /// under the enclosing function's id. An expression body is a `return`
    /// at no site, so a name still held there is refused, not placed.
    fn lambda_frame(&mut self, e: &'a Expr, caps: &[Val]) -> Result<String, Gap> {
        let Expr::Lambda {
            params,
            body,
            line,
            col,
            id: _,
        } = e
        else {
            return gap("a lambda frame of no lambda literal", e.line());
        };
        let decls = self.proto.types();
        let (ptys, ret): (Vec<Type>, Option<Type>) = match self.ty_of(e).ok() {
            // A `lazy T` field's initializer is a nullary closure.
            Some(t) if vyrn_frontend::types::deferred(&t).is_some() => {
                (Vec::new(), vyrn_frontend::types::deferred(&t).cloned())
            }
            Some(t) => match vyrn_frontend::types::resolve(&t, &decls) {
                Type::Fn(ptys, r) => {
                    // A body its slot does not take. [`fn_slot`] covers a
                    // call's parameter; this covers every other slot.
                    if let LambdaBody::Expr(x) = body {
                        let got = self.ty_of(x).ok().filter(|g| *g != Type::Err);
                        let sp = self.body.speech();
                        let refusal =
                            got.and_then(|g| stored_slot(Some(&g), None, &t, &decls, *line, &sp));
                        self.body.mistyped.extend(refusal);
                    }
                    (ptys, Some(*r))
                }
                _ => return gap("a lambda the checker did not type as a function", *line),
            },
            // An untyped literal, an argument of a monomorphized generic: each
            // parameter takes the type of its first use in the typed body.
            None => {
                let vars = mentions_in_lambda(body);
                let ptys = params
                    .iter()
                    .map(|p| {
                        vars.iter()
                            .find(|v| matches!(v, Expr::Var { name, .. } if *name == p.name))
                            .and_then(|v| self.types.get(&v.id()))
                            .cloned()
                            .unwrap_or(Type::Unit)
                    })
                    .collect();
                (ptys, None)
            }
        };
        if ptys.len() != params.len() {
            return gap("a lambda with the wrong arity for its type", *line);
        }
        let file = self.body.file.clone();
        let spellings = self.body.spellings.clone();
        let export = self.body.export;
        let outer = std::mem::replace(
            &mut self.body,
            Body {
                id: None,
                name: String::new(),
                file,
                spellings,
                export,
                names: Vec::new(),
                params: Vec::new(),
                assumes: Vec::new(),
                stmts: Vec::new(),
                lambdas: Vec::new(),
                cands: Vec::new(),
                loop_buffers: Vec::new(),
                unbound_drops: Vec::new(),
                refused: Vec::new(),
                mistyped: Vec::new(),
                ends: HashMap::new(),
                consumes: HashMap::new(),
            },
        );
        self.body.name = lambda_spelling(&outer.name, *line, *col);
        self.body.id = self.fns.id(&self.body.name);
        let mut rebound_names = std::collections::HashSet::new();
        if let LambdaBody::Block(b) = body {
            rebound(b, &mut rebound_names);
        }
        // `appends` stays empty: no store in a lambda body is an append
        // target ([`crate::append::append_candidates`]).
        let outer_frame = std::mem::replace(
            &mut self.frame,
            Frame {
                ret,
                rebound: rebound_names,
                ..Frame::default()
            },
        );
        for c in caps {
            let Val::Name(n) = c else {
                continue;
            };
            let info = &outer.names[n.index()];
            let (source, ty) = (info.source.clone(), info.ty.clone());
            let m = self.name(&source, ty, false, *line);
            // A capture is the enclosing frame's; the kernel refuses a take.
            self.body.names[m.index()].borrow_kind = Some(BorrowKind::Capture);
            self.frame.scope.push((source, m));
            self.body.params.push(m);
        }
        for (p, pt) in params.iter().zip(ptys) {
            let m = self.name(&p.name, pt, false, *line);
            self.body.names[m.index()].borrow_kind = param_borrow(Capability::Read, &p.name, true);
            self.frame.scope.push((p.name.clone(), m));
            self.body.params.push(m);
        }
        let mut stmts = Vec::new();
        let r = match body {
            LambdaBody::Block(b) => self.block(b, &mut stmts),
            LambdaBody::Expr(x) => self.val(x, &mut stmts).map(|v| {
                stmts.push(St::Return {
                    value: Some(v),
                    site: NodeId::NONE,
                    is_try: false,
                    line: *line,
                })
            }),
        };
        cut(&mut stmts);
        self.body.stmts = stmts;
        if let (Some(owes), LambdaBody::Block(_)) = (self.frame.ret.clone(), body) {
            falls_through(&mut self.body, &owes, *line, |owes| {
                rule!(LambdaFallsThrough, owes)
            });
        }
        let frame = std::mem::replace(&mut self.body, outer);
        self.frame = outer_frame;
        r?;
        let key = frame.name.clone();
        self.body.lambdas.push(frame);
        Ok(key)
    }

    /// The names of this body a lambda mentions, as a place or as a callee
    /// (`n -> f(n) + 1` captures `f`), in [`lambda_captures`]'s order, each
    /// resolved by [`Builder::lookup`], not a shadowed one (#483). Not
    /// `ast::mentions_place`: it answers `true` for every name in a block
    /// body, and a capture is a read.
    fn captures(&self, e: &Expr) -> Vec<Val> {
        let Expr::Lambda { params, body, .. } = e else {
            return Vec::new();
        };
        let locals = params.iter().map(|p| p.name.clone()).collect();
        lambda_captures(body, locals, &|n| self.lookup(n).is_some())
            .iter()
            .filter_map(|n| self.lookup(n).map(Val::Name))
            .collect()
    }

    /// [`NameInfo::closure_reads`] for one lambda literal.
    fn closure_reads(&mut self, e: &Expr, caps: &[Val]) -> Option<Vec<Name>> {
        if self.frame.call_keeps.take() == Some(false) {
            return None;
        }
        let Expr::Lambda { body, .. } = e else {
            return None;
        };
        // Mentions, not captures: a captured callee is no value the closure
        // holds.
        let vars = mentions_in_lambda(body);
        Some(
            caps.iter()
                .filter_map(|v| match v {
                    Val::Name(n) => Some(*n),
                    Val::Lit(_) => None,
                })
                .filter(|n| reads_place(&vars, &self.body.names[n.index()].source))
                .collect(),
        )
    }

    /// A read of module state as a value: a borrow of the global, which no
    /// frame may take. `e` is the expression that has the type.
    fn global_read(
        &mut self,
        e: &'a Expr,
        name: &str,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<Val, Gap> {
        self.unknown_at(e);
        let ty = self.ty_of(e)?;
        let t = if self.owns(&ty) {
            self.borrow_name(e, ty, line)
        } else {
            self.temp(ty, line)
        };
        out.push(St::Let(t, Rhs::Read(Place::Global(name.to_string()))));
        Ok(Val::Name(t))
    }

    /// The `consume p` prefix. Its refusals are about the keyword,
    /// which the kernel does not see, so they are stated here.
    fn take_prefix(
        &mut self,
        e: &'a Expr,
        kw: usize,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<Val, Gap> {
        take_names_a_place(e, &self.own.place_names, line, false)?;
        self.consume_names_a_borrow(e, line)?;
        if self.in_module_state(e) {
            return self.read_val(e, out);
        }
        self.take_place_at(e, out, Some((line, kw)))
    }

    /// Whether `e` is module state or a place inside it. Nothing may take module
    /// state, so `consume g.f` reads the place, as `consume g` does, and the
    /// kernel refuses the read where an owner receives it. A take would leave
    /// a hole that the audited teardown frees again (#469).
    fn in_module_state(&self, e: &Expr) -> bool {
        vyrn_frontend::ast::place_path(e).is_some_and(|(root, _)| {
            self.lookup(&root).is_none() && self.program.globals.iter().any(|g| g.name == root)
        })
    }

    /// Refuses a prefix `consume` of a borrow (a `read` or `modify` parameter,
    /// a capture), which would hand somebody else's buffer away. Stated at
    /// the keyword: the write-back of `s.dense.push(i)` reaches
    /// [`Builder::take_place`] with no `consume` and changes no owner. The
    /// refusal names the root, as `movecheck::check_take` does; a heapless
    /// root is no borrow.
    fn consume_names_a_borrow(&self, e: &'a Expr, line: usize) -> Result<(), Gap> {
        let Some((root, path)) = vyrn_frontend::ast::place_path(e) else {
            return Ok(());
        };
        let Some(n) = self.lookup(&root) else {
            return Ok(());
        };
        let info = &self.body.names[n.index()];
        match &info.borrow_kind {
            // The sentence names the root; the fixes name the path.
            Some(k) if info.borrow && !info.must_use_param => {
                let what = k.what(&root);
                refuse(rule!(ConsumedBorrow, root, what), k.fixes(&path), line)
            }
            _ => Ok(()),
        }
    }

    /// Answers a join arm's yield of a name bound outside the construct
    /// (`let rel = if p == "" { st } else { p + "/" + st }`) and outside the
    /// enclosing loop. The yield moves the name, and the kernel releases it
    /// on the edges that do not; inside a loop the move would repeat every
    /// turn, and the `let` that owns the result refuses
    /// ([`Builder::loop_alias`]). A rebind hands the name back and is not
    /// refused.
    ///
    /// `mark` is the name count before the arms were lowered, which tells an
    /// outside name from a payload binder minted inside the arm.
    fn alias_out(
        &self,
        e: &Expr,
        v: &Val,
        mark: usize,
    ) -> Option<(String, Option<(usize, usize)>)> {
        let Val::Name(m) = v else { return None };
        let m = m.index();
        // Only an owning name can be freed twice. A loop variable is minted
        // above the loop mark, so each turn's element is its own.
        (m < mark
            && self.body.names[m].releases
            && self.frame.loop_marks.last().is_some_and(|lm| m < *lm))
        .then(|| (self.body.names[m].source.clone(), self.spelled_end(e)))
    }

    /// A move out of a sub-place: `consume x.f`, or the receiver a rebuilding
    /// builtin hands back (`s.dense.push(i)` is `s.dense = @push(s.dense, i)`).
    /// The value leaves into an owned name and the base keeps a hole.
    fn take_place(&mut self, e: &'a Expr, out: &mut Vec<St>) -> Result<Val, Gap> {
        self.take_place_at(e, out, None)
    }

    /// [`Builder::take_place`], given the line and column of the `consume`
    /// keyword where there is one: a `consume x.f` leaves a hole the base's
    /// release walks around; the write-back form's store fills it. This is
    /// where a binding's holes are stated.
    fn take_place_at(
        &mut self,
        e: &'a Expr,
        out: &mut Vec<St>,
        keyword: Option<(usize, usize)>,
    ) -> Result<Val, Gap> {
        let ty = self.ty_of(e)?;
        let place = self.place(e, out)?;
        self.frame.pending_receiver = None;
        if let Some(kw) = keyword {
            if let Some((n, path)) = crate::kernel::root_of(&place) {
                let fixes = self.uncopy(e, kw);
                let key = (self.frame.stmt, n, path.clone());
                self.body.consumes.entry(key).or_insert(fixes);
                // A hole the walk cannot skip is not stated: a declared
                // `release` cannot be told to leave a field alone, so it would
                // free the field twice (`refusals/r22_drop_with_a_hole.vyrn`).
                let bty = self.body.names[n.index()].ty.clone();
                let rel = path.trim_start_matches('.').to_string();
                if !rel.is_empty()
                    && vyrn_frontend::declared::skippable(
                        &self.own.proto,
                        &bty,
                        std::slice::from_ref(&rel),
                    )
                {
                    let hs = &mut self.body.names[n.index()].holes;
                    if !hs.contains(&path) {
                        hs.push(path);
                        hs.sort();
                    }
                }
            }
        }
        let t = self.temp(ty, e.line());
        out.push(St::Let(t, Rhs::Take(place)));
        self.hold(t);
        Ok(Val::Name(t))
    }

    /// The String `jsonSchema<T>()` renders from `T`'s declaration at compile
    /// time. `direct::Fn_::reflected` renders the same declaration.
    fn schema(&self, e: &Expr) -> Option<Lit> {
        let Expr::Call {
            name, type_args, ..
        } = e
        else {
            return None;
        };
        let [Type::Named(t) | Type::App(t, _)] = type_args.as_slice() else {
            return None;
        };
        let types = self.proto.types();
        let decl = types.get(t).filter(|_| name == "jsonSchema")?;
        Some(Lit::Str(vyrn_frontend::types::json_schema_string(
            decl, types,
        )))
    }

    /// The `impl Show` function a `print`, `@str` or `value` of one argument
    /// calls, where the program declares it.
    fn render_callee(&self, name: &str, args: &[Expr]) -> Option<String> {
        let [a] = args else { return None };
        if !matches!(name, "print" | "@str" | "value") {
            return None;
        }
        let t = self.ty_of(a).ok()?;
        let base = vyrn_frontend::types::resolve(&t, self.proto.types());
        vyrn_frontend::types::show_dispatch(&self.program.impls, &t, &base)
            .filter(|f| self.program.functions.iter().any(|d| &d.name == f))
    }

    /// `@copy` of the read `e`. The copy drains its operand as a call does: a
    /// temporary the read left (`pieces()` of `pieces()[0]`) is dropped once
    /// the copy is bound.
    fn copy_of(&mut self, e: &'a Expr, out: &mut Vec<St>) -> Result<Rhs, Gap> {
        self.frame.drain += 1;
        let v = self.read_at(e, out, None);
        self.frame.drain -= 1;
        self.copy_rhs(v?, e)
    }

    /// The copy of `v`, the value of `e`: the type's `impl Copy` where it has
    /// one, `@copy` otherwise.
    fn copy_rhs(&mut self, v: Val, e: &Expr) -> Result<Rhs, Gap> {
        self.frame.pending_copy = true;
        let copied = (self.copied(e)).and_then(|(f, s)| Some((self.fn_id(&f)?, f, s)));
        let (callee, kind, solved) = match copied {
            Some((id, f, solved)) => (f, Callee::Fn(id), solved),
            None => ("@copy".to_string(), Callee::Reserved, Vec::new()),
        };
        Ok(Rhs::Call {
            callee,
            args: vec![(Arg::Val(v), Capability::Read)],
            write_back: false,
            kind,
            ret: Some(self.ty_of(e)?),
            solved,
            targets: Vec::new(),
        })
    }

    /// Whether `e` is a heap element of a temporary (`pieces()[0]`) or a
    /// heap field under one (`pieces()[0].s`), stated as `@copy` of the read
    /// (#537): the temporary is released whole after the consumer, so the
    /// taker must own a copy. A type with `impl Copy` is copied by the impl
    /// only where the borrow has no lowering: an element itself, not a field
    /// under one and not a scrutinee, which both stay borrows.
    fn copies_a_part(&self, e: &Expr) -> bool {
        let mut at = e;
        while let Expr::Field { expr, .. } = at {
            at = expr;
        }
        matches!(at, Expr::Call { name, args, .. }
            if name == vyrn_frontend::project::AT && args.len() == 2 && !is_place_read(&args[0]))
            && self.ty_of(e).is_ok_and(|t| {
                self.owns(&t)
                    && ((self.program.impls)
                        .method(
                            vyrn_frontend::types::COPY,
                            &t,
                            vyrn_frontend::types::COPY_COPY,
                        )
                        .is_none()
                        || (std::ptr::eq(at, e) && self.frame.scrutinee != Some(e.id())))
            })
    }

    /// Whether `name(args)` at `e` is a read that owns no heap: an element of
    /// a builtin array, a String's byte, or a map's entry, whose `Option` the
    /// runtime's lookup builds. A receiver that is no place is bound to a
    /// temporary and released after the read, as a field's is.
    fn reads_an_element(&self, name: &str, args: &[Expr], e: &Expr) -> bool {
        name == vyrn_frontend::project::AT
            && args.len() == 2
            && self.ty_of(e).is_ok_and(|t| !self.owns(&t))
            && self.ty_of(&args[0]).is_ok_and(|t| {
                matches!(
                    vyrn_frontend::types::resolve(&t, self.proto.types()),
                    Type::Array(_)
                        | Type::ArrayN(..)
                        | Type::SmallArray(..)
                        | Type::Str
                        | Type::Map(..)
                )
            })
    }

    /// An expression as the right-hand side of a `let`. The temporaries its
    /// own reads queued are left in `after_of_rhs` for the binding that
    /// follows; an enclosing expression's are kept aside meanwhile, so a
    /// nested read cannot drop what an outer one is about to read.
    fn rhs(&mut self, e: &'a Expr, out: &mut Vec<St>) -> Result<Rhs, Gap> {
        let outer = std::mem::take(&mut self.frame.after);
        let held = self.frame.held.len();
        let r = self.rhs_inner(e, out);
        self.frame.held.truncate(held);
        let mine = std::mem::replace(&mut self.frame.after, outer);
        self.frame.after_of_rhs = mine;
        r
    }

    /// Releases the temporaries queued in `after` since `mark`, where no
    /// `rhs` drains them: a place read's key (`m["k".copy()]`) or an operand.
    fn drop_since(&mut self, mark: usize, out: &mut Vec<St>) {
        for t in self.frame.after.split_off(mark) {
            out.push(St::Drop(t, Site::None, 0, None));
        }
    }

    /// Holds the temporary `t` until its consumer runs, where it owns heap.
    fn hold(&mut self, t: Name) {
        if self.body.names[t.index()].releases {
            self.frame.held.push(t);
        }
    }

    /// The rows a `?`'s failure exit runs before its return: the enclosing
    /// loops, the held temporaries, then the plan's releases. A keyed
    /// temporary (a scrutinee) is the plan's.
    fn leave_try(&mut self, tid: NodeId, out: &mut Vec<St>) -> Result<(), Gap> {
        self.leave_loops(out);
        for &t in &self.frame.held {
            if self.body.names[t.index()].binding.is_none() {
                out.push(St::Drop(t, Site::None, 0, None));
            }
        }
        self.drops_at(Exit::Try, tid, out)
    }

    /// `a && b` as `if a { b } else { false }`, and `a || b` as
    /// `if a { true } else { b }`, storing into a `Bool` temporary on each
    /// edge. The checker refuses any operand but `Bool`.
    fn short_circuit(
        &mut self,
        op: BinOp,
        lhs: &'a Expr,
        rhs: &'a Expr,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<Rhs, Gap> {
        let res = self.temp(Type::Bool, line);
        let store = |value| St::Store {
            place: Place::Name(res),
            value,
            old: Old::Nothing,
            line,
            site: Site::None,
            releases: false,
            holes: Vec::new(),
        };
        // The left operand runs where the expression does, and the emitter
        // drains its temporaries at the operator (`Fn_::binary`).
        self.frame.drain += 1;
        let cond = self.read_val(lhs, out)?;
        let mark = self.frame.after.len();
        let mut taken = Vec::new();
        let v = self.read_val(rhs, &mut taken)?;
        taken.push(store(v));
        // The right operand's temporaries are released on its edge, the only
        // path that evaluates it.
        self.drop_since(mark, &mut taken);
        self.frame.drain -= 1;
        let decided = vec![store(Val::Lit(Lit::Bool(op == BinOp::Or)))];
        let (then, els) = if op == BinOp::And {
            (taken, decided)
        } else {
            (decided, taken)
        };
        out.push(St::If {
            cond,
            then,
            els,
            site: NodeId::NONE,
        });
        Ok(Rhs::Val(Val::Name(res)))
    }

    fn rhs_inner(&mut self, e: &'a Expr, out: &mut Vec<St>) -> Result<Rhs, Gap> {
        match e {
            // Named again because this match is exhaustive on purpose: a new
            // `Expr` variant must fail to compile. `lit_of` says what each is.
            Expr::Int(_, _)
            | Expr::Byte(_, _)
            | Expr::Float(_, _)
            | Expr::Bool(_, _)
            | Expr::Str(_, _) => match lit_of(e) {
                Some(l) => Ok(Rhs::Val(Val::Lit(l))),
                None => gap("a literal form `lit_of` does not answer", e.line()),
            },
            // A `let` builds a nullary constructor in its own slot, as it
            // builds `Some(v)`; [`Builder::nullary`] names one elsewhere.
            Expr::Var { name, line, id: _ }
                if self.lookup(name).is_none() && self.is_nullary(name) =>
            {
                let ty = self.ty_of(e)?;
                self.call(name, &[], *line, Some(ty), None, out)
            }
            Expr::Var { .. } | Expr::Consume { .. } => Ok(Rhs::Val(self.val(e, out)?)),
            Expr::Unary { op, expr, .. } => Ok(Rhs::Prim(
                Op::Un(*op),
                vec![self.read_val(expr, out)?],
                self.produced(e),
            )),
            Expr::Binary {
                op,
                lhs,
                rhs,
                line,
                id: _,
            } => {
                // `&&` and `||` are control flow: the right operand may not run.
                if matches!(op, BinOp::And | BinOp::Or) {
                    return self.short_circuit(*op, lhs, rhs, *line, out);
                }
                // An operator drains its operands' temporaries in both
                // compiled backends (`binary`, `gen_binary`).
                self.frame.drain += 1;
                // A String `+` is `@concat`, and a comparison and a `=~` read
                // operands the same way, so an allocating operand is an
                // argument at `(@concat, side)`. A `+` concatenates where its
                // own type is `String`.
                let concat = matches!(op, BinOp::Add)
                    && self.ty_of(e).is_ok_and(|t| {
                        matches!(
                            vyrn_frontend::types::resolve(&t, self.proto.types()),
                            Type::Str
                        )
                    });
                let compares = op.compare().is_some();
                let a = if concat || compares || matches!(op, BinOp::Match) {
                    self.read_arg(lhs, out, "@concat", 0)?
                } else {
                    self.read_val(lhs, out)?
                };
                let b = if concat || compares {
                    self.read_arg(rhs, out, "@concat", 1)?
                } else {
                    self.read_val(rhs, out)?
                };
                self.frame.drain -= 1;
                Ok(Rhs::Prim(Op::Bin(*op), vec![a, b], self.produced(e)))
            }
            Expr::Field { expr, field, .. } => {
                if let Some(inner) = self.deferred_of(e) {
                    return Ok(self.force(e, inner, out)?);
                }
                if self.copies_a_part(e) {
                    return self.copy_of(e, out);
                }
                let fty = self.ty_of(e)?;
                let place = self.place(expr, out)?;
                if let Some((r, _, _)) = self.frame.pending_receiver {
                    self.body.names[r.index()].receiver = Some(e.id());
                }
                if self.owns(&fty) {
                    // `let sels = parse(q).sels`: the binding takes the field
                    // out of the unnamed receiver; the kernel sees the rest
                    // held.
                    return Ok(Rhs::Take(Place::Field(Box::new(place), field.clone())));
                }
                Ok(Rhs::Read(Place::Field(Box::new(place), field.clone())))
            }
            Expr::Call {
                dot: _,
                name,
                args,
                line,
                type_args: _,
                id: _,
            } if prelude::builtin(name).is_some_and(|b| b.spec == Some(Spec::Traps)) => {
                let r = self.call(name, args, *line, self.produced(e), None, out)?;
                out.push(St::Do {
                    rhs: r,
                    line: *line,
                    site: NodeId::NONE,
                });
                out.push(St::Trap);
                Ok(Rhs::Val(Val::Lit(Lit::Opaque(Opaque::Trapped))))
            }
            // `xs[i]` of a heapless element, a String's byte and a map entry
            // are reads, not calls.
            Expr::Call {
                dot: _,
                name,
                args,
                line,
                type_args,
                id: _,
            } => {
                if self.reads_an_element(name, args, e) {
                    return Ok(Rhs::Read(self.place(e, out)?));
                }
                if self.copies_a_part(e) {
                    return self.copy_of(e, out);
                }
                // A builtin whose argument names its callee is a call to that
                // function where the program declares it.
                if let Some((f, fwd)) =
                    vyrn_frontend::loader::routed_callee(name, type_args, args, |a| {
                        self.ty_of(a).ok()
                    })
                    .filter(|(f, _)| self.program.functions.iter().any(|d| &d.name == f))
                {
                    return self.call(&f, fwd, *line, self.produced(e), None, out);
                }
                // A render of a type the language does not render calls its
                // `impl Show`. `print` releases the String after; `value`
                // takes it.
                if let Some(f) = self.render_callee(name, args) {
                    let r = self.call(&f, args, *line, Some(Type::Str), None, out)?;
                    if name == "@str" {
                        return Ok(r);
                    }
                    let t = self.temp(Type::Str, *line);
                    out.push(St::Let(t, r));
                    let cap = if name == "value" {
                        Capability::Consume
                    } else {
                        self.frame.after.push(t);
                        Capability::Read
                    };
                    return Ok(Rhs::Call {
                        callee: name.clone(),
                        args: vec![(Arg::Val(Val::Name(t)), cap)],
                        write_back: false,
                        kind: Callee::Reserved,
                        ret: self.produced(e),
                        solved: Vec::new(),
                        targets: Vec::new(),
                    });
                }
                if let Some(l) = self.schema(e) {
                    return Ok(Rhs::Val(Val::Lit(l)));
                }
                // `schemaOf<T>()` is the `Schema` literal it stands for, whose
                // nodes the checker typed (`project::schema_at`).
                if let Some(lit) = self.program.expansions.schema_at(e) {
                    return self.rhs(lit, out);
                }
                let id = e.id();
                // Every call, accepted ones too: the solve binds a caller's
                // own type parameter where a slot names it (#566).
                let solved = self.solved.get(&id).map_or(&[][..], Vec::as_slice);
                let bound = |v: &str| {
                    self.lookup(v).is_some() || self.program.globals.iter().any(|g| g.name == v)
                };
                let decls = self.proto.types();
                let at = fn_slot(
                    self.program,
                    name,
                    args,
                    *line,
                    solved,
                    &self.types,
                    &decls,
                    &bound,
                    &self.body.speech(),
                );
                self.body.mistyped.extend(at);
                let mut r = self.call(name, args, *line, self.produced(e), Some(e), out)?;
                if let Rhs::Call {
                    kind: Callee::Fn(_),
                    solved,
                    targets,
                    ..
                } = &mut r
                {
                    if let Some(s) = self.solved.get(&e.id()) {
                        *solved = s.clone();
                    }
                    let subst: HashMap<String, Type> = solved.iter().cloned().collect();
                    for t in targets.iter_mut() {
                        if let Target::Lambda(_, _, slot) = t {
                            *slot = vyrn_frontend::types::substitute(slot, &subst);
                        }
                    }
                }
                Ok(r)
            }
            Expr::TryConstruct { name, args, .. } => {
                self.unknown_at(e);
                let mut vs = Vec::new();
                for a in args {
                    vs.push(self.val(a, out)?);
                }
                Ok(Rhs::Make(Ctor::Try(name.clone()), vs))
            }
            // A part crosses into its slot's type as a stored value does.
            Expr::ArrayLit { elems, line, id: _ } => {
                let ety = self
                    .ty_of(e)
                    .ok()
                    .and_then(|t| self.elem_ty(&t, *line).ok());
                let mut vs = Vec::new();
                for a in elems {
                    vs.push(self.proven_val(a, ety.as_ref(), *line, out)?);
                }
                Ok(Rhs::Make(Ctor::Array, vs))
            }
            Expr::StructLit {
                name,
                fields,
                line,
                id: _,
            } => {
                let ty = self.ty_of(e).ok();
                let mut vs = Vec::new();
                for (f, a) in fields {
                    let fty = ty.as_ref().and_then(|t| self.field_ty(t, f, *line).ok());
                    vs.push(self.proven_val(a, fty.as_ref(), *line, out)?);
                }
                let to = Type::Named(name.clone());
                if self
                    .proto
                    .types()
                    .get(name)
                    .is_some_and(|d| d.predicate.is_some())
                    && !self.proven(e, &to)
                {
                    self.frame.owed = Some((name.clone(), *line));
                }
                Ok(Rhs::Make(
                    Ctor::Record(
                        name.clone(),
                        fields.iter().map(|(f, _)| f.clone()).collect(),
                    ),
                    vs,
                ))
            }
            Expr::MapLit {
                entries,
                line,
                id: _,
            } => {
                let (kty, vty) = match self
                    .ty_of(e)
                    .map(|t| vyrn_frontend::types::resolve(&t, self.proto.types()))
                {
                    Ok(Type::Map(k, v)) => (Some(*k), Some(*v)),
                    _ => (None, None),
                };
                let mut vs = Vec::new();
                for (k, v) in entries {
                    vs.push(self.proven_val(k, kty.as_ref(), *line, out)?);
                    vs.push(self.proven_val(v, vty.as_ref(), *line, out)?);
                }
                Ok(Rhs::Make(Ctor::Map, vs))
            }
            Expr::IfExpr {
                cond,
                then_branch,
                else_branch,
                line,
                id: _,
            } => {
                let ty = self.ty_of(e)?;
                // The plan keys an if-expression's edge rows by the expression.
                let site = e.id();
                let res = self.temp(ty, *line);
                let c = self.condition(cond, "if", *line, out)?;
                let mark = self.body.names.len();
                let held = self.frame.held.len();
                let mut t = Vec::new();
                let tv = self.val(then_branch, &mut t)?;
                let mut aliased = self.alias_out(then_branch, &tv, mark);
                let then_v = tv.clone();
                t.push(St::Store {
                    place: Place::Name(res),
                    value: tv,
                    old: Old::Nothing,
                    line: *line,
                    site: Site::None,
                    releases: false,
                    holes: Vec::new(),
                });
                self.edge_drops(site, 0, &mut t)?;
                let mut f = Vec::new();
                self.frame.held.truncate(held);
                match else_branch {
                    Some(eb) => {
                        let ev = self.val(eb, &mut f)?;
                        aliased = aliased.or(self.alias_out(eb, &ev, mark));
                        let else_v = ev.clone();
                        f.push(St::Store {
                            place: Place::Name(res),
                            value: ev,
                            old: Old::Nothing,
                            line: *line,
                            site: Site::None,
                            releases: false,
                            holes: Vec::new(),
                        });
                        self.edge_drops(site, 1, &mut f)?;
                        self.join_borrows(res, &[then_v, else_v]);
                        if let Some(a) = aliased {
                            self.frame.loop_aliased.insert(res, a);
                        }
                    }
                    None => return gap("an `if` expression without `else`", *line),
                }
                out.push(St::If {
                    cond: c,
                    then: t,
                    els: f,
                    site,
                });
                Ok(Rhs::Val(Val::Name(res)))
            }
            Expr::Match {
                scrutinee,
                arms,
                line,
                ..
            } => {
                let ty = self.ty_of(e)?;
                let sty = self.ty_of(scrutinee)?;
                let rt = vyrn_frontend::types::resolve(&sty, self.proto.types());
                let mid = e.id();
                let res = self.temp(ty, *line);
                let (sv, consuming) =
                    self.scrutinee(scrutinee, mid, Some(arms_span(*line, arms)), out)?;
                let owns = self.owns_boxes(scrutinee, consuming);
                let outer = self.body.names.len();
                let held = self.frame.held.len();
                let mut core_arms = Vec::new();
                let mut yields = Vec::new();
                for (i, arm) in arms.iter().enumerate() {
                    self.frame.held.truncate(held);
                    let mut body = Vec::new();
                    let mark = self.frame.scope.len();
                    let binds = self.bind_pattern(
                        &arm.pattern,
                        &sty,
                        &rt,
                        consuming,
                        *line,
                        borrow_root(&sv, owns),
                        &mut body,
                    )?;
                    match &arm.body {
                        ArmBody::Expr(ae) => {
                            let v = self.val(ae, &mut body)?;
                            if let Some(a) = self.alias_out(ae, &v, outer) {
                                self.frame.loop_aliased.insert(res, a);
                            }
                            yields.push(v.clone());
                            body.push(St::Store {
                                place: Place::Name(res),
                                value: v,
                                old: Old::Nothing,
                                line: *line,
                                site: Site::None,
                                releases: false,
                                holes: Vec::new(),
                            });
                        }
                        ArmBody::Block(blk) => self.block(blk, &mut body)?,
                    }
                    let frees = self.arm_frees(mid, i as u32, &binds, &mut body);
                    self.edge_drops(mid, i as u32, &mut body)?;
                    self.frame.scope.truncate(mark);
                    core_arms.push(Arm {
                        binds,
                        frees: Some(frees),
                        body,
                        test: self.arm_test(&arm.pattern, &rt, *line)?,
                        site: mid,
                        index: i as u32,
                    });
                }
                self.covers(&core_arms, &rt, *line, out)?;
                self.join_borrows(res, &yields);
                out.push(St::Switch {
                    on: sv,
                    arms: core_arms,
                    consuming,
                    carries: false,
                    owns,
                    site: mid,
                    line: *line,
                });
                self.drops_at(Exit::Scrutinee, mid, out)?;
                Ok(Rhs::Val(Val::Name(res)))
            }
            Expr::Try { expr, line, id: _ } => {
                let ty = self.ty_of(e)?;
                let ity = self.ty_of(expr)?;
                let tid = e.id();
                let res = self.temp(ty, *line);
                let (sv, consuming) = self.scrutinee(expr, tid, None, out)?;
                let owns = self.owns_boxes(expr, consuming);
                let decls = self.proto.types();
                // A declared `Fallible` enum asks its impl. `Option`
                // and `Result` resolve to variant lists too, so they are
                // excluded by name.
                let r = vyrn_frontend::types::resolve(&ity, &decls);
                if matches!(r, Type::Enum(_))
                    && vyrn_frontend::types::option_payload(&r).is_none()
                    && vyrn_frontend::types::result_payloads(&r).is_none()
                {
                    return self.fallible_try(ity, sv, owns, res, tid, out);
                }
                // Failure: the exit's drops, then the propagated value leaves.
                let mut fail = Vec::new();
                let mark = self.frame.scope.len();
                // Each binder is keyed by a node of the `?`: the error by the
                // `?` itself, the value by its operand.
                let fb = self.bind_pattern(
                    &Pattern::Failure(Binder {
                        id: Id::of(tid),
                        ..Binder::synthetic("@err")
                    }),
                    &ity,
                    &r,
                    consuming,
                    *line,
                    borrow_root(&sv, owns),
                    &mut fail,
                )?;
                self.leave_try(tid, &mut fail)?;
                // An `Option` fails with `None` of the frame's result; a
                // `Result` with its error binder taken into `Err`.
                let value = match (fb.first(), self.frame.ret.clone()) {
                    (Some(n), Some(rt)) => {
                        let t = self.temp(rt.clone(), *line);
                        fail.push(St::Let(
                            t,
                            Rhs::Call {
                                callee: "Err".into(),
                                args: vec![(Arg::Val(Val::Name(*n)), Capability::Consume)],
                                write_back: false,
                                kind: Callee::Ctor,
                                ret: Some(rt),
                                solved: Vec::new(),
                                targets: Vec::new(),
                            },
                        ));
                        Some(Val::Name(t))
                    }
                    (Some(n), None) => Some(Val::Name(*n)),
                    (None, Some(rt)) => Some(self.nullary("None", rt, *line, &mut fail)?),
                    (None, None) => None,
                };
                fail.push(St::Return {
                    value,
                    site: tid,
                    is_try: true,
                    line: *line,
                });
                self.frame.scope.truncate(mark);
                let mut ok = Vec::new();
                let mark = self.frame.scope.len();
                let ob = self.bind_pattern(
                    &Pattern::Success(Binder {
                        id: Id::of(expr.id()),
                        ..Binder::synthetic("@ok")
                    }),
                    &ity,
                    &r,
                    consuming,
                    *line,
                    borrow_root(&sv, owns),
                    &mut ok,
                )?;
                ok.push(St::Store {
                    place: Place::Name(res),
                    value: ob
                        .first()
                        .map(|n| Val::Name(*n))
                        .unwrap_or(Val::Lit(Lit::Opaque(Opaque::Unbound))),
                    old: Old::Nothing,
                    line: *line,
                    site: Site::None,
                    releases: false,
                    holes: Vec::new(),
                });
                let ok_frees = self.arm_frees(tid, 1, &ob, &mut ok);
                let fail_frees = self.arm_frees(tid, 0, &fb, &mut fail);
                self.frame.scope.truncate(mark);
                out.push(St::Switch {
                    on: sv,
                    arms: vec![
                        Arm {
                            frees: Some(fail_frees),
                            binds: fb,
                            body: fail,
                            test: Test::Tag(0),
                            site: tid,
                            index: 0,
                        },
                        Arm {
                            frees: Some(ok_frees),
                            binds: ob,
                            body: ok,
                            test: Test::Tag(1),
                            site: tid,
                            index: 1,
                        },
                    ],
                    consuming,
                    carries: false,
                    owns,
                    site: tid,
                    line: *line,
                });
                Ok(Rhs::Val(Val::Name(res)))
            }
            Expr::Lambda { .. } => {
                let caps = self.captures(e);
                // Waits for the name `bind` gives ([`Builder::pending_closure`]).
                self.frame.pending_closure = self.closure_reads(e, &caps);
                let key = self.lambda_frame(e, &caps)?;
                Ok(Rhs::Prim(Op::Closure(key), caps, self.produced(e)))
            }
        }
    }

    /// `?` on a declared `Fallible` type: the failing path returns
    /// the whole value; the succeeding path hands it to the impl's `success`,
    /// which reads it and answers a value of its own, then releases it. Each
    /// arm accounts for the value exactly once.
    fn fallible_try(
        &mut self,
        ity: Type,
        sv: Val,
        owns: bool,
        res: Name,
        tid: NodeId,
        out: &mut Vec<St>,
    ) -> Result<Rhs, Gap> {
        let line = self.body.names[res.index()].line;
        let Some(key) = vyrn_frontend::types::type_key(&ity) else {
            return gap("a `?` on a type with no impl key", line);
        };
        let method = |m: &str| {
            vyrn_frontend::types::impl_method_name(vyrn_frontend::types::FALLIBLE, &key, m)
        };
        let success = method("success");
        let (Some(imp), Some(is_success), Some(success_id)) = (
            self.program.impls.get(vyrn_frontend::types::FALLIBLE, &key),
            self.fn_id(&method("isSuccess")),
            self.fn_id(&success),
        ) else {
            return gap("a `?` on a type with no `Fallible` impl", line);
        };
        // The impl's `isSuccess` chooses the arm. Both impl calls are declared
        // functions under the dispatched name and read their argument.
        let held = self.temp(Type::Bool, line);
        out.push(St::Let(
            held,
            Rhs::Call {
                solved: self
                    .impl_args(imp, &method("isSuccess"), &ity)
                    .unwrap_or_default(),
                callee: method("isSuccess"),
                args: vec![(Arg::Val(sv.clone()), Capability::Read)],
                write_back: false,
                kind: Callee::Fn(is_success),
                ret: Some(Type::Bool),
                targets: Vec::new(),
            },
        ));
        let failed = self.temp(Type::Bool, line);
        out.push(St::Let(
            failed,
            Rhs::Prim(Op::Un(UnOp::Not), vec![Val::Name(held)], Some(Type::Bool)),
        ));
        let mut fail = Vec::new();
        self.leave_try(tid, &mut fail)?;
        fail.push(St::Return {
            value: Some(sv.clone()),
            site: tid,
            is_try: true,
            line,
        });
        let mut ok = Vec::new();
        let t = self.temp(self.body.names[res.index()].ty.clone(), line);
        ok.push(St::Let(
            t,
            Rhs::Call {
                solved: self.impl_args(imp, &success, &ity).unwrap_or_default(),
                callee: success,
                args: vec![(Arg::Val(sv.clone()), Capability::Read)],
                write_back: false,
                kind: Callee::Fn(success_id),
                ret: Some(self.body.names[res.index()].ty.clone()),
                targets: Vec::new(),
            },
        ));
        if let Val::Name(n) = sv {
            if owns && self.body.names[n.index()].releases {
                ok.push(St::Drop(n, Site::None, 0, None));
            }
        }
        ok.push(St::Store {
            place: Place::Name(res),
            value: Val::Name(t),
            old: Old::Nothing,
            line,
            site: Site::None,
            releases: false,
            holes: Vec::new(),
        });
        out.push(St::Switch {
            on: sv,
            arms: vec![
                Arm {
                    frees: Some(Vec::new()),
                    binds: Vec::new(),
                    body: fail,
                    test: Test::Holds(failed),
                    site: tid,
                    index: 0,
                },
                Arm {
                    frees: Some(Vec::new()),
                    binds: Vec::new(),
                    body: ok,
                    test: Test::Else,
                    site: tid,
                    index: 1,
                },
            ],
            consuming: false,
            carries: false,
            owns,
            site: tid,
            line,
        });
        Ok(Rhs::Val(Val::Name(res)))
    }

    /// A place, for a read or a store. A field chain over a name, an element
    /// of one, or a temporary the expression produced (an unnamed receiver).
    fn place(&mut self, e: &'a Expr, out: &mut Vec<St>) -> Result<Place, Gap> {
        match e {
            Expr::Var { name, .. } => match self.lookup(name) {
                Some(n) => Ok(Place::Name(n)),
                None => {
                    self.unknown_at(e);
                    Ok(Place::Global(name.clone()))
                }
            },
            // A `lazy` field's place holds the thunk; a read through it is a
            // read of the forced value (`h.f.n`).
            Expr::Field { expr, field, .. } if self.deferred_of(e).is_none() => {
                let base = self.place(expr, out)?;
                Ok(Place::Field(Box::new(base), field.clone()))
            }
            Expr::Call {
                name, args, line, ..
            } if name == "@at" && args.len() == 2 => {
                if let Some(p) = self.inlined("at", &args[0], &args[1..], *line, out)? {
                    return Ok(p);
                }
                let bty = self.ty_of(&args[0])?;
                let base = self.place(&args[0], out)?;
                // The receiver is this read's; a field read in the index would
                // release it as its own.
                let receiver = self.frame.pending_receiver.take();
                let i = self.read_val(&args[1], out)?;
                self.frame.pending_receiver = receiver;
                if self.is_map(&bty) {
                    Ok(Place::Key(Box::new(base), i))
                } else {
                    Ok(Place::Elem(Box::new(base), i))
                }
            }
            _ => {
                let v = self.val(e, out)?;
                match v {
                    Val::Name(t) => {
                        if self.body.names[t.index()].releases {
                            // [`NameInfo::receiver_malloc`]: a callee's block.
                            let malloc = matches!(
                                e,
                                Expr::Call { name, .. } if !name.starts_with('@')
                            );
                            self.frame.pending_receiver = Some((t, e.id(), malloc));
                        }
                        Ok(Place::Name(t))
                    }
                    // A literal receiver (`"abc".byteLength`): a temporary
                    // bound to the literal gives the chain a base.
                    Val::Lit(_) => {
                        let ty = self.ty_of(e)?;
                        let t = self.temp(ty, e.line());
                        out.push(St::Let(t, Rhs::Val(v)));
                        Ok(Place::Name(t))
                    }
                }
            }
        }
    }

    /// The call `name(args)` producing `ret`. `dest` is the call expression
    /// where the caller has one: a constructor's payload slots take their types
    /// from its checked type (`Some(lit)` into `Option<Key>`).
    fn call(
        &mut self,
        name: &str,
        args: &'a [Expr],
        line: usize,
        ret: Option<Type>,
        dest: Option<&'a Expr>,
        out: &mut Vec<St>,
    ) -> Result<Rhs, Gap> {
        // `a[i]` asks the receiver's type for `at` before any builtin row, as
        // `Checker::call` dispatches it.
        if let (vyrn_frontend::project::AT, [recv, rest @ ..]) = (name, args) {
            if let Some(p) = self.inlined("at", recv, rest, line, out)? {
                return Ok(Rhs::Read(p));
            }
        }
        // `value(s)` of a String: the box owns its payload (#512), so a String
        // read out of a place is copied first.
        if let ("value", [arg]) = (name, args) {
            let string = self
                .ty_of(arg)
                .is_ok_and(|t| vyrn_frontend::types::resolve(&t, self.proto.types()) == Type::Str);
            if string {
                let v = if prelude::boxes_a_copy(arg, string) {
                    let copy = self.call("@copy", args, line, Some(Type::Str), None, out)?;
                    let c = self.temp(Type::Str, line);
                    self.bind(c, copy, out);
                    Val::Name(c)
                } else {
                    self.val(arg, out)?
                };
                return Ok(Rhs::Call {
                    callee: name.to_string(),
                    args: vec![(Arg::Val(v), Capability::Consume)],
                    write_back: false,
                    kind: Callee::Reserved,
                    ret,
                    solved: Vec::new(),
                    targets: Vec::new(),
                });
            }
        }
        let decls = self.proto.types();
        // A method call takes the capabilities of the impl it dispatches to:
        // two protocols may declare one method name with different ones.
        let method = (self.program.impls.iter())
            .any(|i| i.methods.iter().any(|m| m.name == name))
            .then(|| self.dispatched(name, args.first()?))
            .flatten()
            .and_then(|(f, solved)| Some((self.fn_id(&f)?, f, solved)));
        // A seeded row whose result is its receiver's own type hands the
        // buffer back through the result, so the receiver is taken by the
        // call.
        let rebuilds = prelude::rebuilds(name);
        // Who the callee is decides each argument position's capability.
        let mut kind = Callee::Reserved;
        let mut member = None;
        // A binding of function type is asked first, as `Checker::call` asks
        // it: a `fn`-typed parameter `h` shadows a function `h` the program
        // declares, and `h(req)` is a call through the value.
        let bound = self.lookup(name).filter(|n| {
            matches!(
                vyrn_frontend::types::resolve(&self.body.names[n.index()].ty, decls),
                Type::Fn(..)
            )
        });
        let mut caps: Vec<Capability> = if let Some(n) = bound {
            // A function value reads every argument: the checker refuses a
            // target that takes one otherwise (`Checker::reads_every_param`).
            kind = Callee::Value(n);
            vec![Capability::Read; args.len()]
        } else if let Some(id) = self.fn_id(name) {
            kind = Callee::Fn(id);
            let f = &self.program.functions[id.index()];
            f.params.iter().map(|p| p.capability).collect()
        } else if prelude::signature(name).is_some() {
            kind = Callee::Builtin;
            // The emitter turns a routed builtin into a call to its target, so
            // no `Callee::Fn` row stands for `check::clause_guards` to write a
            // clause check at. `core::check::clauses` assumes the target has no
            // parameter clause; only the prelude's route table names a target,
            // so a program cannot break the assumption.
            debug_assert!(
                prelude::builtin(name).is_none_or(|b| {
                    (b.route.iter().chain(&b.gen_route)).all(|r| {
                        let target = self.program.functions.iter().filter(|f| f.name == *r);
                        target.flat_map(|f| &f.params).all(|p| p.clause.is_none())
                    })
                }),
                "a route to a function with a parameter clause skips its call-site check"
            );
            let mut caps: Vec<Capability> = (0..args.len())
                .map(|i| prelude::capability(name, i).unwrap_or(Capability::Read))
                .collect();
            if rebuilds && !caps.is_empty() {
                caps[0] = Capability::Consume;
            }
            caps
        } else if let Some((id, ..)) = method {
            kind = Callee::Method;
            let m = &self.program.functions[id.index()];
            m.params.iter().map(|p| p.capability).collect()
        } else if let Some(p) = self.projection(name) {
            kind = Callee::Projection;
            p.params.iter().map(|p| p.capability).collect()
        } else if let Some((id, sig)) = (args.first()).and_then(|r| self.protocol_member(name, r)) {
            // A protocol member no impl answers, called on a bounded type
            // parameter in a generic read as written: its signature is what
            // a caller reads (`MethodSig::recv`).
            kind = Callee::Method;
            member = Some(id);
            std::iter::once(sig.recv)
                .chain(sig.param_caps.iter().copied())
                .collect()
        } else if matches!(name, "Some" | "None" | "Ok" | "Err") || self.is_variant(name) {
            kind = Callee::Ctor;
            vec![Capability::Consume; args.len()]
        } else if vyrn_frontend::checker::RESERVED.contains(&name)
            || vyrn_frontend::ast::is_surface_builtin(name)
        {
            // A reserved name with no prelude row (`fromJson`, `value`, a
            // generation-time surface builtin, `ast::SURFACE_BUILTINS`): the
            // prelude's capability where it has one, `read` elsewhere.
            (0..args.len())
                .map(|i| prelude::capability(name, i).unwrap_or(Capability::Read))
                .collect()
        } else if decls.contains_key(name) {
            kind = match args {
                [v] if decls[name].predicate.is_some()
                    && self.proven(v, &Type::Named(name.to_string())) =>
                {
                    Callee::Proven
                }
                _ => Callee::Named,
            };
            vec![Capability::Consume; args.len()]
        } else if name.starts_with('@') {
            vec![Capability::Read; args.len()]
        } else if matches!(name, "print") {
            vec![Capability::Read; args.len()]
        } else if matches!(
            name,
            vyrn_frontend::checker::GEN_REFLECT
                | vyrn_frontend::checker::GEN_NEXT_INT
                | vyrn_frontend::checker::GEN_NEXT_STR
        ) {
            // The generation host's primitives, which exist only
            // in a generator host (`Host::gen`). The host reads what it is
            // handed, so the guest keeps every argument it owns.
            vec![Capability::Read; args.len()]
        } else if let Some(g) = self.program.globals.iter().find(|g| {
            g.name == name
                && matches!(
                    vyrn_frontend::types::resolve(&g.ty.clone().unwrap_or(Type::Unit), decls),
                    Type::Fn(..)
                )
        }) {
            // Module state of function type: borrowed out of the
            // global and called through, as a forced `lazy` field is.
            let ty = g.ty.clone().unwrap_or(Type::Unit);
            let n = self.name(name, ty, false, line);
            out.push(St::Let(n, Rhs::Read(Place::Global(name.to_string()))));
            kind = Callee::Value(n);
            vec![Capability::Read; args.len()]
        } else {
            return gap_d("a call this slice cannot attribute", name, line);
        };
        if caps.len() < args.len() {
            return gap("a call with more arguments than parameters", line);
        }
        // The capability row an argument's release reads: the impl a method
        // call dispatches to, the protocol member it resolved, else the name.
        let of = match (kind, method.as_ref().map(|m| m.0), member) {
            (Callee::Fn(id), ..) | (Callee::Method, Some(id), _) => CapsOf::Fn(id),
            (Callee::Method, None, Some(m)) => CapsOf::Method(m),
            (Callee::Value(_), ..) => CapsOf::None,
            _ => self.own.arg_caps.named(name),
        };
        let length = prelude::builtin(name).map(|b| b.length);
        if let (Some(prelude::Length::ShrinksByOneIfNotEmpty), Some(recv)) = (length, args.first())
        {
            self.shrinks(&name[1..], recv, line);
        }
        if let (Callee::Projection, Some(recv)) = (kind, args.first()) {
            if let Some(p) = self.inlined(name, recv, &args[1..], line, out)? {
                return Ok(Rhs::Read(p));
            }
        }
        let mut vs = Vec::new();
        let mut temps_to_drop = Vec::new();
        // [`Rhs::Call`]'s `write_back`.
        let write_back = rebuilds
            && caps.first() == Some(&Capability::Consume)
            && matches!(args.first(), Some(Expr::Var { .. }));
        // A call drains its arguments' temporaries after it runs, unless its
        // result points into one of them: a lending call leaves them to the
        // call or operator above (both backends' `call` drain).
        let lends_here = self.lends_name(name) || self.hands_back_a_borrow(name, args);
        if vyrn_frontend::movecheck::hands_back(name)
            && !lends_here
            && !caps.is_empty()
            && args
                .first()
                .is_some_and(|a| self.ty_of(a).is_ok_and(|t| self.owns(&t)))
        {
            // The result is the argument: the temporary is taken and released
            // once, as the result.
            caps[0] = Capability::Consume;
        }
        // A stream is linear: a position it fills takes it, whatever
        // the position's word says.
        for (c, a) in caps.iter_mut().zip(args) {
            if self
                .ty_of(a)
                .is_ok_and(|t| matches!(vyrn_frontend::types::resolve(&t, decls), Type::Stream(_)))
            {
                *c = Capability::Consume;
            }
        }
        // `@list([..])` moves the literal's elements into the array it builds,
        // so it takes the literal; a read would release them twice.
        if name == "@list" {
            caps.iter_mut().for_each(|c| *c = Capability::Consume);
        }
        let drains = !lends_here;
        if drains {
            self.frame.drain += 1;
        }
        let bound = match kind {
            Callee::Fn(_) => self.targets_of(name, args),
            _ => Vec::new(),
        };
        let param_tys: Vec<Type> = match (kind, dest) {
            (Callee::Fn(id), _) => (self.program.functions[id.index()].params.iter())
                .map(|p| p.ty.clone())
                .collect(),
            (Callee::Ctor, Some(dest)) => {
                let dest = self.ty_of(dest).ok();
                let decls = self.proto.types();
                let variants = dest
                    .as_ref()
                    .map(|d| vyrn_frontend::types::resolved(d, decls));
                match variants.as_deref() {
                    Some(Type::Enum(vs)) => {
                        (vs.iter().find(|v| v.name == name)).map(|v| v.payload.clone())
                    }
                    _ => None,
                }
                .unwrap_or_default()
            }
            _ => Vec::new(),
        };
        let mut targets = Vec::new();
        // A lambda target's captures and a stored value follow the call's own
        // arguments, where [`specialize`] puts a forwarded target's.
        let mut forwarded = Vec::new();
        for (k, (a, cap)) in args.iter().zip(caps.iter()).enumerate() {
            let forwards = match bound.get(k) {
                Some(Some(t @ Target::Value(_))) => {
                    targets.push(t.clone());
                    true
                }
                Some(Some(t)) => {
                    if let Target::Lambda(..) = t {
                        let vals = self.captures(a);
                        self.lambda_frame(a, &vals)?;
                        forwarded.extend(vals.into_iter().map(|v| (Arg::Val(v), Capability::Read)));
                    }
                    targets.push(t.clone());
                    continue;
                }
                _ => false,
            };
            // Whether this position may keep a lambda literal written at it
            // (`declared::arg_cap`); an unanswered position may, the safe
            // direction. A lambda deeper in the argument gets `None` and
            // escapes: a literal retains what it is given.
            self.frame.call_keeps = matches!(a, Expr::Lambda { .. })
                .then(|| (self.own.arg_caps.at(of, k)).is_none_or(|c| c == Capability::Consume));
            let global = matches!(a, Expr::Var { name, .. }
                if self.lookup(name).is_none()
                    && self.program.globals.iter().any(|g| &g.name == name));
            // Module state handed to `modify` (`insert(cells, v)`) is passed
            // as the place: a borrow of it would be written through.
            if let (Capability::Modify, Expr::Var { name, .. }, true) = (cap, a, global) {
                vs.push((Arg::Place(Place::Global(name.clone())), *cap));
                continue;
            }
            // A removal's receiver that is a field or an element
            // (`r.xs.pop()`) is passed as the place: the call shrinks the
            // array where it lies.
            if let (0, true) = (k, prelude::removes(name) && !matches!(a, Expr::Var { .. })) {
                let at = self.modify_place(a, line, out)?;
                vs.push((Arg::Place(at.0), *cap));
                continue;
            }
            let proven = param_tys.get(k).and_then(|to| self.proven_crossing(a, to));
            let v = if let Some(to) = proven {
                // A proven crossing is the constructor row, so no reader
                // checks it again.
                let t = self.checked_temp(&to, a, line, out)?;
                if self.arg_released(a, t, name, of, k) {
                    self.body.names[t.index()].arg_drop = Some(a.id());
                }
                Val::Name(t)
            } else if *cap == Capability::Consume {
                // The write-back form on module state (`books.push(b)`), a
                // field or an element: the receiver leaves the place and the
                // store after the call fills it.
                if k == 0
                    && rebuilds
                    && (global || !matches!(a, Expr::Var { .. } | Expr::Consume { .. }))
                    && is_place_read(a)
                {
                    self.take_place(a, out)?
                } else {
                    self.val(a, out)?
                }
            } else {
                self.read_at(a, out, Some((name, of, k)))?
            };
            self.frame.call_keeps = None;
            if let Val::Name(t) = v {
                // Queue the drop `read_arg`'s key stands for. The key also
                // stands on a borrow (a forced `lazy` field); only a name
                // this frame releases is dropped.
                let info = &self.body.names[t.index()];
                if info.releases && info.arg_drop.is_some() && !self.frame.after.contains(&t) {
                    temps_to_drop.push(t);
                }
            }
            match forwards {
                true => forwarded.push((Arg::Val(v), *cap)),
                false => vs.push((Arg::Val(v), *cap)),
            }
        }
        vs.extend(forwarded);
        if drains {
            self.frame.drain -= 1;
        }
        self.frame.after.extend(temps_to_drop);
        // `Int32(n)` and its siblings convert between scalars. The operand is
        // read above like any argument, so the keying and drains stay.
        if let (Some(to), [(Arg::Val(v), _)]) = (
            vyrn_frontend::types::numeric_conv_target(name),
            vs.as_slice(),
        ) {
            return Ok(Rhs::Prim(Op::Conv(to), vec![v.clone()], ret));
        }
        // `@concat(a, b)` is the String `+` the interpolation spine spells as
        // a call, so the row is the operator's, over the same arguments.
        if let ("@concat", [(Arg::Val(a), _), (Arg::Val(b), _)]) = (name, vs.as_slice()) {
            return Ok(Rhs::Prim(
                Op::Bin(BinOp::Add),
                vec![a.clone(), b.clone()],
                ret,
            ));
        }
        // A method is a call after dispatch, and so is `x.copy()` of a type
        // with `impl Copy`.
        let dispatched = match (kind, args.first()) {
            (Callee::Method, _) => method,
            (Callee::Reserved, Some(r)) if name == "@copy" => {
                (self.copied(r)).and_then(|(f, s)| Some((self.fn_id(&f)?, f, s)))
            }
            _ => None,
        };
        let (callee, kind, solved) = match dispatched {
            Some((id, f, solved)) => (f, Callee::Fn(id), solved),
            None => (name.to_string(), kind, Vec::new()),
        };
        Ok(Rhs::Call {
            callee,
            args: vs,
            write_back,
            kind,
            ret,
            solved,
            targets,
        })
    }

    /// The impl function the method `name` dispatches to on `recv`'s type,
    /// and its type arguments ([`Builder::impl_args`]): the one function the
    /// program declares under a name some protocol with that method mangles.
    /// A receiver that is a bounded type parameter as written dispatches
    /// through the protocol its bound names ([`Builder::protocol_member`]).
    fn dispatched(&self, name: &str, recv: &Expr) -> Option<(String, Vec<(String, Type)>)> {
        let rty = self.ty_of(recv).ok()?;
        let key = vyrn_frontend::types::type_key(&rty)?;
        let bound = (self.protocol_member(name, recv))
            .map(|(id, _)| &self.program.protocols[id.protocol as usize].name);
        let fs: std::collections::BTreeMap<String, Vec<(String, Type)>> = (self.program.impls)
            .of_key(&key)
            .filter(|i| i.methods.iter().any(|m| m.name == name))
            .filter(|i| bound.is_none_or(|p| &i.protocol == p))
            .filter_map(|i| {
                let f = vyrn_frontend::types::impl_method_name(&i.protocol, &key, name);
                Some((f.clone(), self.impl_args(i, &f, &rty)?))
            })
            .collect();
        let mut fs = fs.into_iter();
        match (fs.next(), fs.next()) {
            (Some(f), None) => Some(f),
            _ => None,
        }
    }

    /// The `impl Copy` function `x.copy()` calls on `recv`'s type, and its
    /// type arguments.
    fn copied(&self, recv: &Expr) -> Option<(String, Vec<(String, Type)>)> {
        let rty = self.ty_of(recv).ok()?;
        let impls = &self.program.impls;
        let f = impls.method(
            vyrn_frontend::types::COPY,
            &rty,
            vyrn_frontend::types::COPY_COPY,
        )?;
        let imp = impls.get(
            vyrn_frontend::types::COPY,
            &vyrn_frontend::types::type_key(&rty)?,
        )?;
        let solved = self.impl_args(imp, &f, &rty)?;
        Some((f, solved))
    }

    /// Whether the program declares `f` as a non-generic function.
    fn concrete_fn(&self, f: &str) -> bool {
        self.program
            .functions
            .iter()
            .any(|g| g.name == f && g.type_params.is_empty())
    }

    /// Per argument of a call to `name`, the [`Target`] a `fn`-typed
    /// parameter is bound to: a declared function, a bound parameter of this
    /// body, a lambda literal with its captures, or a stored value
    /// forwarded as its one capture. A lambda at a `consume` position is a
    /// stored value. Empty where the callee takes no function. A parameter is
    /// `fn`-typed as written; one of an alias type takes a stored value.
    fn targets_of(&self, name: &str, args: &[Expr]) -> Vec<Option<Target>> {
        let Some(f) = self.program.functions.iter().find(|f| f.name == name) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (p, a) in f.params.iter().zip(args) {
            if !matches!(p.ty, Type::Fn(..)) {
                out.push(None);
                continue;
            }
            let value = Target::Value(p.name.clone());
            let t = match a {
                // At a `consume` position the literal falls to the last arm:
                // a value the kernel judges ([`NameInfo::closure_reads`]).
                Expr::Lambda { line, col, .. } if p.capability != Capability::Consume => {
                    let caps = (self.captures(a).into_iter())
                        .filter_map(|c| match c {
                            Val::Name(n) => Some(n),
                            Val::Lit(_) => None,
                        })
                        .map(|n| {
                            let info = &self.body.names[n.index()];
                            (info.source.clone(), info.ty.clone())
                        })
                        .collect();
                    let key = lambda_spelling(&self.body.name, *line, *col);
                    Target::Lambda(key, caps, p.ty.clone())
                }
                Expr::Var { name: v, .. } => match self.lookup(v) {
                    Some(n)
                        if self.body.params.contains(&n)
                            && matches!(self.body.names[n.index()].ty, Type::Fn(..)) =>
                    {
                        Target::Param(n)
                    }
                    Some(_) => value,
                    None if self.concrete_fn(v) => Target::Fn(v.clone()),
                    None => return Vec::new(),
                },
                _ => value,
            };
            out.push(Some(t));
        }
        if out.iter().all(Option::is_none) {
            return Vec::new();
        }
        out
    }

    /// Whether a bare name no binding holds is a nullary constructor: `None`
    /// or a fieldless variant.
    fn is_nullary(&self, name: &str) -> bool {
        name == "None" || self.is_variant(name)
    }

    fn is_variant(&self, name: &str) -> bool {
        let decls = self.proto.types();
        decls.values().any(|d| {
            vyrn_frontend::types::declared_variants(&d.base)
                .is_some_and(|vs| vs.iter().any(|v| v.name == name))
        })
    }
}

vyrn_frontend::body_scope_descent!(BodyVisit, body_block, body_stmt, body_expr);

/// The binding a place expression names: `s.id[0]` names `s`.
fn place_base(name: &str) -> &str {
    &name[..name.find(['.', '[']).unwrap_or(name.len())]
}

/// Whether a lambda body's mentions read `base` or a place under it.
fn reads_place(vars: &[&Expr], base: &str) -> bool {
    vars.iter().any(|v| match v {
        Expr::Var { name, .. } => {
            name == base
                || (name.len() > base.len()
                    && name.starts_with(base)
                    && matches!(name.as_bytes()[base.len()], b'.' | b'['))
        }
        _ => false,
    })
}

/// The captured local variables of a lambda body, in first-seen
/// order: names read in the body that are not in `locals` (the lambda's
/// parameters and its own bindings) and that `is_local` answers for as an
/// enclosing local. This is the one statement of the capture order: the
/// closure row lists its captures in it, and the emitter lifts the signature
/// in it.
pub fn lambda_captures(
    body: &LambdaBody,
    locals: std::collections::HashSet<String>,
    is_local: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    struct CapturesOf<'a> {
        out: Vec<String>,
        seen: std::collections::HashSet<String>,
        is_local: &'a dyn Fn(&str) -> bool,
    }

    impl CapturesOf<'_> {
        fn take(&mut self, n: &str, locals: &std::collections::HashSet<String>) {
            if locals.contains(n) || self.seen.contains(n) {
                return;
            }
            // The lifted function reaches module state and functions directly.
            if (self.is_local)(n) {
                self.seen.insert(n.to_string());
                self.out.push(n.to_string());
            }
        }
    }

    impl BodyVisit<'_> for CapturesOf<'_> {
        fn expr(&mut self, e: &Expr, locals: &std::collections::HashSet<String>) -> bool {
            match e {
                Expr::Var { name, .. } => self.take(place_base(name), locals),
                // A call captures a callee that names an enclosing local:
                // `|req, ps| run(req)` over a `fn`-typed `run` calls a value,
                // not a symbol. `is_local` is false for a top-level function.
                Expr::Call { name, .. } => self.take(name, locals),
                // A lambda body holds no lambda literal.
                Expr::Lambda { .. } => return false,
                _ => {}
            }
            true
        }
    }

    let mut v = CapturesOf {
        out: Vec::new(),
        seen: std::collections::HashSet::new(),
        is_local,
    };
    let mut locals = locals;
    match body {
        LambdaBody::Expr(e) => body_expr(e, &locals, &mut v),
        LambdaBody::Block(b) => body_block(b, &mut locals, &mut v),
    }
    v.out
}

/// Every `Var` node in a lambda's body, nested lambdas included, minus the
/// names the body itself binds: where an untyped parameter's type can be
/// read, and which captures the closure reads as values. A shadowed name is
/// not recorded: a capture is a read, and counting one would refuse a program
/// that reads nothing.
fn mentions_in_lambda(body: &LambdaBody) -> Vec<&Expr> {
    struct Mentions<'e>(Vec<&'e Expr>);

    impl<'e> BodyVisit<'e> for Mentions<'e> {
        fn expr(&mut self, e: &'e Expr, locals: &std::collections::HashSet<String>) -> bool {
            if let Expr::Var { name, .. } = e {
                if !locals.contains(place_base(name)) {
                    self.0.push(e);
                }
            }
            true
        }
    }

    let mut v = Mentions(Vec::new());
    let mut locals = std::collections::HashSet::new();
    match body {
        LambdaBody::Expr(e) => body_expr(e, &locals, &mut v),
        LambdaBody::Block(b) => body_block(b, &mut locals, &mut v),
    }
    v.0
}

/// The checker's type for the expression at `node`, off the record `own` is
/// lowered against. `None` for a node the checker never typed.
fn node_ty(own: &Ownership, node: NodeId) -> Option<Type> {
    own.record.node_types.get(&node).cloned()
}

/// The type arguments the checker solved at the call `node`, as the body
/// writes them: before an instance's substitution, each parameter by its
/// written name.
fn node_solved(own: &Ownership, node: NodeId) -> Option<Vec<(String, Type)>> {
    use vyrn_frontend::{ast::written_param, types::written_params};
    let (_, s) = own.record.node_substs.get(&node)?;
    Some(
        (s.iter())
            .map(|(p, t)| {
                (
                    written_param(p).to_string(),
                    written_params(t).unwrap_or_else(|| t.clone()),
                )
            })
            .collect(),
    )
}

/// The declaration the checker recorded at the call `node` it typed `Err`
/// for the typed judgment ([`vyrn_frontend::checker::Recorded::calls`]).
fn call_decl(own: &Ownership, node: NodeId) -> Option<vyrn_frontend::checker::CallDecl> {
    own.record.calls.get(&node).cloned()
}

/// The name a lambda literal at `line` and `col` inside the body named
/// `outer` is built and emitted under, and so its key in
/// [`crate::World::body_of`].
pub fn lambda_spelling(outer: &str, line: usize, col: usize) -> String {
    format!("{outer}@lambda:{line}:{col}")
}

/// The line and column of a lambda key [`lambda_spelling`] spelled, which
/// is how a lambda source is named.
pub fn lambda_at(name: &str) -> Option<(usize, usize)> {
    let (_, at) = name.rsplit_once("@lambda:")?;
    let (line, col) = at.split_once(':')?;
    Some((line.parse().ok()?, col.parse().ok()?))
}

/// The instance of `body` whose `fn`-typed parameters are bound:
/// each parameter in `bound` leaves the parameter list, a call through it is
/// [`Callee::Bound`] to its target, and a call that passes it on names that
/// target. A lambda target's captures take the parameter's place; a call
/// through it passes them first, and a call passing it on takes them after
/// its own arguments (`direct::ho_args`). A bound parameter read once as a
/// value is made where it is read: its target's closure variant. `None`
/// where one is read twice, or a pass-through names no target.
pub fn specialize(body: &Body, bound: &[(Name, Target)]) -> Option<Body> {
    if bound.iter().any(|(_, t)| matches!(t, Target::Param(_))) {
        return None;
    }
    let mut out = body.clone();
    let values: Vec<Name> = (bound.iter())
        .filter_map(|(n, t)| match t {
            Target::Value(source) => {
                out.names[n.index()].source = source.clone();
                Some(*n)
            }
            _ => None,
        })
        .collect();
    let mut caps: Vec<(Name, Vec<Name>)> = Vec::new();
    for (n, t) in bound {
        let Target::Lambda(_, cs, _) = t else {
            continue;
        };
        let names = (cs.iter())
            .map(|(source, ty)| {
                let mut info = out.names[n.index()].clone();
                info.source = source.clone();
                info.ty = ty.clone();
                out.names.push(info);
                Name((out.names.len() - 1) as u32)
            })
            .collect();
        caps.push((*n, names));
    }
    bind_targets(&mut out.stmts, bound, &caps);
    let mut reads = vec![0; out.names.len()];
    count_reads(&out.stmts, &mut reads);
    let gone = |n: &Name| !values.contains(n);
    for (n, t) in bound
        .iter()
        .filter(|(n, _)| gone(n) && reads[n.index()] > 0)
    {
        let parts = (caps.iter().find(|(c, _)| c == n))
            .map(|(_, ns)| ns.iter().map(|c| Val::Name(*c)).collect())
            .unwrap_or_default();
        // A target with captures makes a value that owns its capture box, and
        // the lambda that captures it holds a copy, so it is released after.
        let release = !Vec::is_empty(&parts);
        if release {
            let info = &mut out.names[n.index()];
            (info.releases, info.heap, info.borrow, info.borrow_kind) = (true, true, false, None);
        }
        let made = St::Let(*n, Rhs::Make(Ctor::Closure(t.clone()), parts));
        if reads[n.index()] > 1 || !make_before_read(&mut out.stmts, reads.len(), *n, made, release)
        {
            return None;
        }
    }
    out.params = (out.params.iter())
        .flat_map(|p| match caps.iter().find(|(n, _)| n == p) {
            Some((_, names)) => names.clone(),
            None if gone(p) && bound.iter().any(|(n, _)| n == p) => Vec::new(),
            None => vec![*p],
        })
        .collect();
    Some(out)
}

/// Puts `made`, the row that binds `n`, before the one row that reads `n`, at
/// that row's depth, and with `release` a release of `n` after it. `names` is
/// the body's name count. `false` where no row reads it, and where `release`
/// and the read is no lambda's capture, which copies what it reads.
fn make_before_read(ss: &mut Vec<St>, names: usize, n: Name, made: St, release: bool) -> bool {
    let mut reads = vec![0; names];
    for i in 0..ss.len() {
        let nested: Vec<&mut Vec<St>> = ss[i].lists_mut().collect();
        let mut inner = 0;
        for b in &nested {
            count_reads(b, &mut reads);
            inner += std::mem::take(&mut reads[n.index()]);
        }
        if inner > 0 {
            return nested
                .into_iter()
                .find(|b| {
                    count_reads(b, &mut reads);
                    std::mem::take(&mut reads[n.index()]) > 0
                })
                .is_some_and(|b| make_before_read(b, names, n, made, release));
        }
        count_reads(std::slice::from_ref(&ss[i]), &mut reads);
        if reads[n.index()] > 0 {
            if release {
                if !matches!(ss[i], St::Let(_, Rhs::Prim(Op::Closure(_), ..))) {
                    return false;
                }
                ss.insert(i + 1, St::Drop(n, Site::None, 0, None));
            }
            ss.insert(i, made);
            return true;
        }
    }
    false
}

fn bind_targets(ss: &mut [St], bound: &[(Name, Target)], caps: &[(Name, Vec<Name>)]) {
    each_row_mut(ss, &mut |s| {
        let (St::Let(_, rhs) | St::Do { rhs, .. }) = s else {
            return;
        };
        let Rhs::Call {
            callee,
            args,
            kind,
            targets,
            ..
        } = rhs
        else {
            return;
        };
        if let Some(v) = kind.value() {
            match bound.iter().find(|(n, _)| *n == v) {
                Some((_, Target::Fn(f))) => {
                    *kind = Callee::Bound;
                    *callee = f.clone();
                }
                Some((_, Target::Lambda(key, ..))) => {
                    *kind = Callee::Bound;
                    *callee = key.clone();
                    let names = caps.iter().find(|(n, _)| *n == v).map(|(_, ns)| ns);
                    let lead = names.into_iter().flatten();
                    let lead = lead.map(|c| (Arg::Val(Val::Name(*c)), Capability::Read));
                    args.splice(0..0, lead.collect::<Vec<_>>());
                }
                Some((_, Target::Value(source))) => *callee = source.clone(),
                _ => {}
            }
        }
        for t in targets.iter_mut() {
            let Target::Param(p) = t else { continue };
            let p = *p;
            let Some((_, to)) = bound.iter().find(|(n, _)| *n == p) else {
                continue;
            };
            *t = to.clone();
            // A stored value forwards itself, the parameter that
            // stays under its name.
            if matches!(to, Target::Value(_)) {
                args.push((Arg::Val(Val::Name(p)), Capability::Read));
            }
            let forwarded = caps.iter().find(|(n, _)| *n == p).map(|(_, ns)| ns);
            let forwarded = forwarded.into_iter().flatten();
            args.extend(forwarded.map(|c| (Arg::Val(Val::Name(*c)), Capability::Read)));
        }
    });
}

/// The declared `release` bodies the row `s` runs where it stands: a drop or
/// a release row runs its name's, and a store runs those of the value it
/// displaces, which has the stored value's type.
pub fn runs<'a>(s: &St, names: &'a [NameInfo]) -> &'a [String] {
    let n = match s {
        St::Drop(n, ..) | St::Row { name: n, .. } => *n,
        St::Store {
            value: Val::Name(n),
            old: Old::Released | Old::Pending | Old::Unreleased,
            ..
        } => *n,
        _ => return &[],
    };
    &names[n.index()].runs
}

/// Each name a `let` of `ss` binds, at the row of `ss` its extent ends at.
///
/// A name holds its value from its `let` to the last row that names it,
/// itself or under it. A release names what it releases, so a name that owns
/// heap holds to its release. A name read out of a place may hold the
/// place's address, so the place's root holds as long as the name. A name
/// that `occurs` (from [`Body::occurrences`]) counts outside `ss` ends at no
/// row of `ss`.
pub fn extent_ends(ss: &[St], occurs: &[u32]) -> Vec<Vec<Name>> {
    let mut seen: HashMap<Name, (usize, u32)> = HashMap::new();
    let mut ns = Vec::new();
    for (i, s) in ss.iter().enumerate() {
        ns.clear();
        names_in(s, &mut ns);
        for &n in &ns {
            let e = seen.entry(n).or_insert((i, 0));
            *e = (i, e.1 + 1);
        }
    }
    let mut end: HashMap<Name, Option<usize>> = seen
        .iter()
        .map(|(&n, &(i, k))| (n, (k == occurs[n.index()]).then_some(i)))
        .collect();
    for s in ss.iter().rev() {
        if let St::Let(n, Rhs::Read(p)) = s {
            let held = end.get(n).copied().flatten();
            if let Some(Val::Name(r)) = root_name(p) {
                if let Some(e) = end.get_mut(&r) {
                    *e = e.zip(held).map(|(a, b)| a.max(b));
                }
            }
        }
    }
    let mut out = vec![Vec::new(); ss.len()];
    for s in ss {
        if let St::Let(n, _) = s {
            if let Some(i) = end.get(n).copied().flatten() {
                out[i].push(*n);
            }
        }
    }
    out
}

/// The parameter every `return` of `body` yields: the parameter itself or a
/// name a `let` bound to it, where no row stores a whole value into any of
/// those names. `None` when a `return` yields anything else, so a body with
/// another result keeps its own. The emitter passes a `consume` parameter
/// this names in the caller's storage (`docs/memory.md`).
pub fn returned_param(body: &Body) -> Option<Name> {
    let from: HashMap<Name, Name> = rows(&body.stmts)
        .filter_map(|(r, _)| match r {
            St::Let(n, Rhs::Val(Val::Name(x))) => Some((*n, *x)),
            _ => None,
        })
        .collect();
    // Each step goes from a `let` to a name bound before it, so a chain has at
    // most `from.len()` steps.
    let root = |mut n: Name| {
        for _ in 0..=from.len() {
            match from.get(&n) {
                Some(&x) => n = x,
                None => return n,
            }
        }
        n
    };
    let mut back = None;
    for (r, _) in rows(&body.stmts) {
        match r {
            St::Return {
                value: Some(Val::Name(n)),
                is_try: false,
                ..
            } if back.is_none_or(|p| p == root(*n)) => back = Some(root(*n)),
            St::Return { .. } => return None,
            _ => {}
        }
    }
    let back = back.filter(|p| body.params.contains(p))?;
    let stored = rows(&body.stmts)
        .any(|(r, _)| matches!(r, St::Store { place: Place::Name(n), .. } if root(*n) == back));
    (!stored).then_some(back)
}

/// The headers a read in `ss` walks: an element read, or a length read,
/// straight off a name, module state, or a chain of fields of one. With
/// `rebase`, each such read of the first reads the name instead. A store and
/// a take keep their place, so a store into an element writes the container
/// and not its header.
fn header_reads(ss: &mut [St], rebase: Option<(&Place, Name)>, out: &mut Vec<Place>) {
    fn place(p: &mut Place, rebase: Option<(&Place, Name)>, out: &mut Vec<Place>) {
        let header = match p {
            Place::Elem(b, _) => Some(b),
            Place::Field(b, f) if f == "length" || f == "byteLength" => Some(b),
            _ => None,
        };
        if let Some(b) = header.filter(|b| fixed(b)) {
            match rebase {
                Some((from, to)) if **b == *from => **b = Place::Name(to),
                Some(_) => {}
                None => out.push((**b).clone()),
            }
            return;
        }
        match p {
            Place::Field(b, _) | Place::Elem(b, _) | Place::Key(b, _) => place(b, rebase, out),
            Place::Name(_) | Place::Global(_) => {}
        }
    }
    each_row_mut(ss, &mut |s| {
        if let St::Let(_, Rhs::Read(p))
        | St::Do {
            rhs: Rhs::Read(p), ..
        } = s
        {
            place(p, rebase, out);
        }
    });
}

/// Whether `p` is a name, module state, or a chain of fields of one: a place
/// no element index moves.
fn fixed(p: &Place) -> bool {
    match p {
        Place::Name(_) | Place::Global(_) => true,
        Place::Field(b, _) => fixed(b),
        Place::Elem(..) | Place::Key(..) => false,
    }
}

/// Every name `s` binds, at any depth: a `let` and a switch arm's binders.
pub fn names_bound(s: &St, out: &mut Vec<Name>) {
    for (r, _) in s.rows() {
        match r {
            St::Let(n, _) => out.push(*n),
            St::Switch { arms, .. } => arms.iter().for_each(|a| out.extend(&a.binds)),
            _ => {}
        }
    }
}

/// Every name a statement of `b` stores into whole (`x = v`), in nested
/// blocks and statement-position `match` arms too. A rebuild's write-back
/// (`xs.push(v)`, `xs = push(xs, v)`) is no such store: it hands the receiver
/// back, and the kernel refuses it on a borrow.
fn rebound(b: &Block, out: &mut std::collections::HashSet<String>) {
    for s in &b.stmts {
        match s {
            Stmt::Assign {
                value: Expr::Call { name: f, args, .. },
                name,
                ..
            } if vyrn_frontend::prelude::rebuilds(f)
                && matches!(args.first(), Some(Expr::Var { name: r, .. }) if r == name) => {}
            Stmt::Assign { name, .. } => {
                out.insert(name.clone());
            }
            Stmt::If {
                then_block,
                else_block,
                ..
            } => {
                rebound(then_block, out);
                if let Some(e) = else_block {
                    rebound(e, out);
                }
            }
            Stmt::While { body, .. } | Stmt::ForIn { body, .. } | Stmt::Region { body, .. } => {
                rebound(body, out)
            }
            Stmt::Expr(Expr::Match { arms, .. }, _) => {
                for a in arms {
                    if let ArmBody::Block(b) = &a.body {
                        rebound(b, out);
                    }
                }
            }
            _ => {}
        }
    }
}

/// The kernel spells a hole `.f.g`; every table spells it `f.g`, relative to
/// the binding.
fn plan_holes(holes: &[String]) -> Vec<String> {
    holes
        .iter()
        .map(|h| h.trim_start_matches('.').to_string())
        .collect()
}

/// Folds one frame's statements into the side table. Called after the placer
/// has added every row, so this is the core the emitters run.
fn fold_facts(body: &Body, proto: &Owned, out: &mut Facts) {
    for (s, _) in rows(&body.stmts) {
        match s {
            St::Store {
                releases,
                site: Site::Node(at),
                old,
                ..
            } => {
                out.stores.insert(*at, *releases);
                if matches!(old, Old::Transferred | Old::Nothing) {
                    out.stood_down.insert(*at);
                }
            }
            St::Drop(_, _, line, _) if *line > 0 => {}
            St::Drop(n, at, _, holes) => match at {
                Site::Node(at) => {
                    if body.names[n.index()].for_consume {
                        out.loop_gives_back.insert(*at);
                    } else {
                        out.discarded.insert(*at);
                    }
                }
                Site::Edge(join, edge) => {
                    let name = body.names[n.index()].source.clone();
                    let holes = plan_holes(body.drop_holes(*n, holes));
                    let rows = out.edges.entry(*join).or_default();
                    // One row per name and edge: a generic instantiated twice
                    // folds the same join twice when the two share a node.
                    if !rows.iter().any(|(r, e, _)| *r == name && e == edge) {
                        rows.push((name, *edge, holes));
                    }
                }
                Site::None => {}
            },
            St::Switch {
                arms,
                consuming: took,
                owns,
                ..
            } => {
                if let Some(a) = arms.first() {
                    out.consuming.insert(a.site, *took);
                    if *owns {
                        out.owns_scrutinee.insert(a.site);
                    }
                }
                for a in arms {
                    if let Some(frees) = &a.frees {
                        let rows: Vec<(String, Vec<String>, Option<DropKind>)> = frees
                            .iter()
                            .map(|b| {
                                let info = &body.names[b.index()];
                                (
                                    info.source.clone(),
                                    plan_holes(&info.holes),
                                    proto.release_kind(&info.ty),
                                )
                            })
                            .collect();
                        // An entry even when empty: "owes none" differs from
                        // "not stated".
                        out.arms.entry((a.site, a.index)).or_default().extend(rows);
                    }
                }
            }
            _ => {}
        }
    }
}

/// The declared type of the module-state name `g`, or the checker's type for
/// its initializer.
fn global_ty(program: &Program, own: &Ownership, g: &str) -> Option<Type> {
    let d = program.globals.iter().find(|d| d.name == g)?;
    d.ty.clone().or_else(|| node_ty(own, d.init.id()))
}

/// `body` with its check rows, each decided with the callees' facts `sums`
/// states unless the build keeps them all ([`crate::check::mode`]): the form
/// an emitter reads.
pub fn checked(
    program: &Program,
    own: &Ownership,
    body: &Body,
    sums: &crate::elide::Summaries,
) -> Body {
    let mut out = stated(program, own, body);
    if decides() {
        crate::elide::decide(&mut out, own.proto.types(), sums);
    }
    out
}

pub(crate) fn decides() -> bool {
    *crate::check::mode() != crate::check::Mode::Keep
}

/// `body` with its check rows, every one kept.
fn stated(program: &Program, own: &Ownership, body: &Body) -> Body {
    let global = |g: &str| global_ty(program, own, g);
    let mut out = body.clone();
    crate::check::state(
        &mut out,
        &crate::check::Types {
            decls: own.proto.types(),
            global: &global,
            fns: &program.functions,
        },
    );
    out
}

/// Every frame's answers of `top`, added to the table, each frame numbered
/// first ([`Fns::number`]).
fn fold_frames(
    program: &Program,
    top: &mut Body,
    own: &Ownership,
    out: &mut Facts,
    fns: &mut Fns,
    bodies: &mut HashMap<FnId, Option<Stated>>,
) {
    let ids = fns.number(top);
    for (body, id) in top.frames().into_iter().zip(ids) {
        fold_frame(program, body, id, own, out, bodies);
    }
}

fn fold_frame(
    program: &Program,
    body: &Body,
    id: FnId,
    own: &Ownership,
    out: &mut Facts,
    bodies: &mut HashMap<FnId, Option<Stated>>,
) {
    let proto = &own.proto;
    // Filled at the same site as the fold, so a body the fold does not see is
    // one no emitter may walk either.
    bodies
        .entry(id)
        .and_modify(|had| *had = None)
        .or_insert_with(|| {
            Some(Stated {
                body: stated(program, own, body),
                decided: Default::default(),
            })
        });
    fold_facts(body, proto, out);
    out.loop_buffer_only
        .extend(body.loop_buffers.iter().copied());
    let released: std::collections::HashSet<Name> = (rows(&body.stmts))
        .filter_map(|(s, _)| match s {
            St::Drop(n, ..) => Some(*n),
            _ => None,
        })
        .collect();
    for (i, info) in body.names.iter().enumerate() {
        if !released.contains(&Name(i as u32)) {
            continue;
        }
        if let Some(node) = info.receiver {
            out.receivers.insert(node, plan_holes(&info.holes));
            if info.receiver_malloc {
                out.receiver_malloc.insert(node);
            }
        }
    }
    for info in body.names.iter() {
        // Whether or not this pass releases the temporary: a `lazy` field read
        // binds a borrow, and the caller still frees the value.
        if let Some(node) = info.arg_drop {
            out.arg_drops.insert(node);
        }
    }
}

/// Whether the kernel's hard refusals fail the command: true unless
/// `VYRN_NO_KERNEL=1`. The knob is for bisecting where a refusal came from;
/// nothing is built under it.
pub fn refuses() -> bool {
    !std::env::var("VYRN_NO_KERNEL").is_ok_and(|v| v == "1")
}

/// Judges one built body with the typed judgment and answers whether it
/// refused. The judgment memo serves only the kernel's refusals, so a refused
/// body is built and judged again next time. `as_written` is false for an
/// instance of a generic function, whose types are the instance's.
fn typed(
    program: &Program,
    own: &Ownership,
    r: &mut Refused,
    top: &Body,
    file: &Option<String>,
    as_written: bool,
) -> bool {
    let global_mutable = |g: &str| program.globals.iter().any(|d| d.name == g && d.mutable);
    let global_ty = |g: &str| global_ty(program, own, g);
    let projected = |t: &Type| program.impls.place(t, "atSet").is_some();
    let ruled_within = |t: &Type, path: &[&Place]| ruled_within(&own.proto, t, path);
    let grouped = |t: &Type, path: &[&Place]| grouped(&own.proto, t, path);
    let rules = crate::typed::StoreRules {
        global_mutable: &global_mutable,
        global_ty: &global_ty,
        projected: &projected,
        ruled_within: &ruled_within,
        grouped: &grouped,
    };
    let (out, seen) = (&mut r.typed, &mut r.seen);
    let mut found = crate::typed::stores(top, &rules, seen);
    found.extend(crate::typed::loops(top, seen));
    // One sentence per line: a declaration's predicate is also the body
    // of its constructor, and the instances of a generic function share
    // their groups.
    let (mut groups, ended) = crate::typed::groups(top, &rules, &[]);
    // The facts come from the check rows, which only an emitter's body
    // states, so a body with a group states its own copy.
    if ended {
        let mut body = stated(program, own, top);
        let mut refuted = Vec::new();
        body.each_frame_mut(&mut |f| {
            refuted.push(crate::elide::refuted(f, own.proto.types()));
        });
        groups = crate::typed::groups(&body, &rules, &refuted).0;
    }
    for u in crate::typed::refused(top, as_written)
        .into_iter()
        .chain(groups)
    {
        let said = |d: &Diagnostic| (&d.file, d.line, &d.message) == (file, u.0, &u.1);
        if !out.iter().any(said) && !found.contains(&u) {
            found.push(u);
        }
    }
    if as_written {
        found.extend(crate::typed::drops(top, program, own.proto.types()));
    }
    found.sort_by_key(|(line, _)| *line);
    let refused = !found.is_empty();
    out.extend(
        found.into_iter().map(|(line, message)| {
            Diagnostic::error(line, 0, "check", message).in_file(file.clone())
        }),
    );
    refused
}

/// The record type with a `where` rule that `path`, taken from a value of type
/// `ty`, passes through before its last place ([`crate::typed::StoreRules`]).
/// A store into an element of an array field passes through: it keeps the
/// field's length, and the rule reads the field through its length alone.
fn ruled_within(own: &Owned, ty: &Type, path: &[&Place]) -> Option<String> {
    ruled_steps(own, ty, path)
        .into_iter()
        .next()
        .map(|(_, n)| n)
}

/// The type a store into a field of a record name belongs to a group of
/// ([`crate::typed::groups`]): the name's own type, when it is the one record
/// with a `where` rule the path passes through.
fn grouped(own: &Owned, ty: &Type, path: &[&Place]) -> Option<String> {
    match ruled_steps(own, ty, path).as_slice() {
        [(0, n)] => Some(n.clone()),
        _ => None,
    }
}

/// The record name and type of a store into `place` that belongs to a group
/// ([`grouped`]).
fn group_of(own: &Owned, names: &[NameInfo], place: &Place) -> Option<(Name, String)> {
    let (Place::Name(c), path) = crate::typed::split(place) else {
        return None;
    };
    Some((*c, grouped(own, &names[c.index()].ty, &path)?))
}

/// Each step of `path` that leaves a record type with a `where` rule
/// ([`ruled_within`]), by its index, with the type.
fn ruled_steps(own: &Owned, ty: &Type, path: &[&Place]) -> Vec<(usize, String)> {
    let decls = own.types();
    let mut out = Vec::new();
    let mut at = ty.clone();
    for (k, step) in path.iter().enumerate() {
        if let Type::Named(n) = &at {
            let pred = decls.get(n).and_then(|d| d.predicate.as_ref());
            if let (Some(pred), Some(fields)) =
                (pred, vyrn_frontend::types::record_fields(&at, decls))
            {
                let elem_of_length_only = match (step, path.get(k + 1)) {
                    (Place::Field(_, f), Some(Place::Elem(..))) => {
                        fields.iter().any(|x| {
                            &x.name == f && vyrn_frontend::types::resolve(&x.ty, decls).is_seq()
                        }) && !vyrn_frontend::consteval::whole_reads(pred).contains(f)
                    }
                    _ => false,
                };
                if !elem_of_length_only {
                    out.push((k, n.clone()));
                }
            }
        }
        let next = match (step, vyrn_frontend::types::resolve(&at, decls)) {
            (Place::Field(_, f), _) => vyrn_frontend::types::record_fields(&at, decls)
                .and_then(|fs| fs.iter().find(|x| &x.name == f).map(|x| x.ty.clone())),
            (Place::Elem(..), t) => t.elem().cloned(),
            (Place::Key(..), Type::Map(_, v)) => Some(*v),
            _ => None,
        };
        let Some(next) = next else { break };
        at = next;
    }
    out
}

/// The row that checks record name `n` against its type `to`'s `where` rule:
/// the constructor reads it, and traps as at a boundary.
fn rule_check(to: String, n: Name, line: usize) -> St {
    St::Do {
        rhs: Rhs::Call {
            ret: Some(Type::Named(to.clone())),
            callee: to,
            args: vec![(Arg::Val(Val::Name(n)), Capability::Read)],
            write_back: false,
            kind: Callee::Named,
            solved: Vec::new(),
            targets: Vec::new(),
        },
        line,
        site: NodeId::NONE,
    }
}

/// Reports a body the core did not build. A gap with a rule is the program's
/// refusal, in the checker's sentence. A gap without one is a defect in the
/// builder: the checker typed the body, so every judgment over the core
/// would otherwise pass over it in silence.
fn refuse_gap(g: Gap, file: &Option<String>, body: &str, r: &mut Refused) {
    let Some(d) = g.rule else {
        let (what, detail) = (g.what, g.detail);
        let rule = match detail.is_empty() {
            true => rule!(CoreGap, what, body),
            false => rule!(CoreGapAt, what, detail, body),
        };
        // The typed judgment's list prints whichever pass refused.
        let d = Diagnostic::refusal(g.line, 0, "check", rule).in_file(file.clone());
        r.typed.push(d);
        return;
    };
    r.kernel.push(Refusal {
        diagnostic: d.in_file(file.clone()),
        body: body.to_string(),
    });
}

/// The kernel's refusals and the typed judgment's, as `augment` gathers them.
#[derive(Default)]
struct Refused {
    kernel: Vec<Refusal>,
    typed: Vec<Diagnostic>,
    /// The statements the typed judgment refused.
    seen: std::collections::HashSet<NodeId>,
}

/// Places the releases the plan did not place. For every body the core can
/// build, the kernel walks it in placement mode: where an owned name is still
/// held at an exit (a block's end, a `return`, a `?`, a `break`, a
/// `continue`) and the plan placed no release, a row is added at that exit,
/// keyed as the plan keys its own, and the binding enters the plan's
/// droppable table. The core orders across a loop's back edge, which the
/// plan's fold cannot.
///
/// Run by [`crate::analyze`], so every consumer of the plan sees the same
/// rows. A body the core cannot build, or the kernel
/// refuses for another reason (a double free, a use after release), is left
/// as the plan had it.
pub fn augment(program: &Program, w: &mut World, judging: bool) {
    let own = &mut w.ownership;
    let mut r = Refused::default();
    let _p = vyrn_frontend::prof::phase("placer");
    // The judgment memo, when the host armed one (`movecheck::Judgments`) and
    // this is the refusal analysis: a body whose key is unchanged is served
    // its refusals, neither built nor judged, unless the effect judgment
    // answers its frames otherwise. An armed host reads only refusals, not
    // the facts or rows, so its lowering may leave out the facts of a body it
    // has walked ([`crate::walked`]).
    let js = vyrn_frontend::prof::phase("placer: judgments");
    let memo = judging
        .then(|| vyrn_frontend::movecheck::Judgments::open(program))
        .flatten();
    drop(js);
    let lw = vyrn_frontend::prof::phase("placer: lower_with");
    let lowered = match memo {
        Some(_) => crate::lower_reusing(program, own),
        None => crate::lower_with(program, own),
    };
    drop(lw);
    w.fns = Fns::lowered(&lowered);
    // `VYRN_KERNEL_TRACE=1` prints every release the placer found owed, and
    // whether it could place it.
    let trace = std::env::var("VYRN_KERNEL_TRACE").is_ok();
    let mut added: Added = Added::new();
    // Every body built, for the `Facts` fold below, and the functions this
    // pass wrote a row for. A row's node belongs to one function, so only
    // those need a rebuild.
    let mut built: Vec<Option<Body>> = Vec::with_capacity(lowered.instances.len());
    let mut touched: std::collections::HashSet<FnId> = Default::default();
    own.accumulators = crate::append::global_append_candidates(program);
    let mut names = NameMemo::default();
    // Every body is built before any is placed: the kernel asks the effect
    // judgment, which joins every body, whether a callee writes module state.
    // A served body gives the judgment its frames as last judged. `test` and
    // `bench` bodies are judged like any other.
    let pending: Vec<Pending> = (lowered.instances.iter().map(Job::Inst))
        .chain(lowered.bodies.iter().map(Job::Outside))
        .map(|job| {
            let key = memo.as_ref().and_then(|m| job.key(m));
            let served = serve(memo.as_ref(), key.as_ref());
            Pending { job, key, served }
        })
        .collect();
    let (shared, fns): (&Ownership, &Fns) = (own, &w.fns);
    // Typing expanded every projection site a first build reads.
    let sealed = program.expansions.seal();
    let firsts = vyrn_frontend::par::in_parallel(
        &pending,
        |p| p.served.as_ref().map_or(p.job.weight(), |_| 0),
        NameMemo::default,
        |names, p| (p.served.is_none()).then(|| p.job.build(program, shared, fns, names)),
    );
    drop(sealed);
    // In job order, so every row a lambda frame takes comes out as on one
    // thread.
    let mut states: Vec<JobState> = Vec::with_capacity(pending.len());
    for (Pending { job, key, served }, first) in pending.into_iter().zip(firsts) {
        let made = match served {
            Some((key, judgment)) => Made::Served(key, judgment),
            None => {
                // `first` is `Some` for every job `served` does not hold.
                let mut top = first.unwrap_or_else(|| job.build(program, own, &w.fns, &mut names));
                if let Ok(b) = &mut top {
                    w.fns.number(b);
                }
                Made::Built(key, top)
            }
        };
        states.push(JobState {
            job,
            made,
            kept: None,
        });
    }
    // The call relation, from the first build of every body, in job order;
    // the writes below add the bodies built for the judgment alone.
    let by_name = crate::by_name(program);
    let mut calls: HashMap<FnId, Vec<FnId>> = HashMap::new();
    for s in &states {
        if let Made::Built(_, Ok(top)) = &s.made {
            crate::world::add_callees(top, &by_name, calls.entry(s.job.id()).or_default());
        }
    }
    let ej = vyrn_frontend::prof::phase("placer: effects");
    let tops: Vec<(&str, &Body)> = states.iter().filter_map(JobState::built).collect();
    // A body that did not build gives the judgment nothing, served or not.
    let late: Vec<(&str, &[Walked])> = states.iter().filter_map(JobState::answered).collect();
    let places = build_places(program, &lowered, own, &mut w.fns);
    let ((mut state, read, answers, reached, allocating), uses) = crate::effects::judge_built(
        program,
        &lowered,
        own,
        &mut w.fns,
        &places,
        &tops,
        &late,
        |judged, reach, refs, top, served_at| {
            let rows = |at: usize, n: usize| -> Vec<Vec<(String, Vec<String>)>> {
                (at..at + n).map(|i| judged.state_callees(i)).collect()
            };
            // What the memo keeps of each body built with a key.
            let read: Vec<Option<Kept>> = (top.iter().zip(&tops))
                .zip(states.iter().filter(|s| s.built().is_some()))
                .map(|((t, (_, b)), s)| {
                    let Made::Built(Some(_), _) = &s.made else {
                        return None;
                    };
                    let n = b.frames().len();
                    let frames = judged.frames[*t..*t + n].iter().map(|f| (*f).clone());
                    Some((frames.collect(), rows(*t, n)))
                })
                .collect();
            let answers: Vec<_> = (served_at.iter().zip(&late))
                .map(|(at, (_, frames))| rows(*at, frames.len()))
                .collect();
            let built = (states.iter().filter(|s| s.built().is_some())).zip(top);
            let served = (states.iter().filter(|s| s.answered().is_some())).zip(served_at);
            let reached: Vec<_> = (built.chain(served))
                .filter_map(|(s, at)| match s.job {
                    Job::Inst(inst) if !inst.func.is_gen => {
                        Some((inst.func.module.clone(), reach.effects[*at]))
                    }
                    _ => None,
                })
                .collect();
            let allocs = |at: usize| judged.effects[at].has(vyrn_frontend::effects::Effect::Alloc);
            // A served body is judged too, at the frame `served_at` names.
            let served_files = (states.iter().filter(|s| s.answered().is_some()))
                .zip(served_at)
                .filter(|(_, at)| allocs(**at))
                .filter_map(|(s, _)| Some((s.job.id(), s.job.module().as_deref()?)));
            let mut files: HashMap<&str, Arc<str>> = HashMap::new();
            let allocating: HashMap<FnId, Arc<str>> = (refs.iter().enumerate())
                .filter(|(at, _)| allocs(*at))
                .filter_map(|(_, b)| Some((b.id?, b.file.as_deref()?)))
                .chain(served_files)
                .map(|(id, file)| {
                    let shared = files.entry(file).or_insert_with(|| Arc::from(file));
                    (id, Arc::clone(shared))
                })
                .collect();
            (judged.state_table(refs), read, answers, reached, allocating)
        },
    );
    drop((tops, late));
    w.reached = reached;
    w.allocating = allocating;
    w.state_uses = uses;
    for (s, r) in (states.iter_mut().filter(|s| s.built().is_some())).zip(read) {
        s.kept = r;
    }
    // A served verdict read the answer the judgment gave its frames then. A
    // body that gets another answer now is built and judged again, before
    // `own` holds the answer, as on its first build.
    for (s, rows) in (states.iter_mut().filter(|s| s.answered().is_some())).zip(answers) {
        let Made::Served(key, served) = &s.made else {
            continue;
        };
        if served.state == rows {
            continue;
        }
        let (key, frames) = (key.clone(), served.frames.clone());
        let mut top = s.job.build(program, own, &w.fns, &mut names);
        if let Ok(b) = &mut top {
            w.fns.number(b);
            crate::world::add_callees(b, &by_name, calls.entry(s.job.id()).or_default());
            for (f, r) in b.frames().iter().zip(&rows) {
                if let (Some(id), false) = (f.id, r.is_empty()) {
                    state.insert(id, r.clone());
                }
            }
        }
        s.kept = Some((frames, rows));
        s.made = Made::Built(Some(key), top);
    }
    own.state_callees = state;
    drop(ej);
    // A hoist asked `kernel::writes` before the effect judgment was held. A
    // frame that hoisted a header and calls a function that stores module
    // state may get a different answer, so it is built again.
    for s in &mut states {
        let Made::Built(_, top) = &mut s.made else {
            continue;
        };
        let unjudged = top.as_ref().is_ok_and(|b| {
            b.frames().iter().any(|f| {
                f.names.iter().any(|i| i.walked == Some(Walk::While))
                    && f.id.is_some_and(|f| own.state_callees.contains_key(&f))
            })
        });
        if unjudged {
            *top = s.job.build(program, own, &w.fns, &mut names);
        }
    }
    // The kernel's walk reads the body and the judgment alone, so it runs on
    // any thread; its rows land below, in job order.
    let shared: &Ownership = own;
    let placed = vyrn_frontend::par::in_parallel(
        &states,
        |s| match &s.made {
            Made::Built(_, Ok(b)) => b.frames().iter().map(|f| rows(&f.stmts).count()).sum(),
            _ => 0,
        },
        || (),
        |(), s| match &s.made {
            Made::Built(_, Ok(b)) => placements(b, &shared.state_callees),
            _ => Vec::new(),
        },
    );
    let mut outside: Vec<Option<Body>> = Vec::with_capacity(lowered.bodies.len());
    for (
        JobState {
            job: j,
            made: m,
            kept,
        },
        placed,
    ) in states.into_iter().zip(placed)
    {
        let j = &j;
        let into = match j {
            Job::Inst(_) => &mut built,
            Job::Outside(_) => &mut outside,
        };
        let (key, made) = match m {
            Made::Served(key, judgment) => {
                r.kernel.extend(judgment.verdict.iter().cloned());
                if let Some(memo) = &memo {
                    memo.tally(true);
                    memo.put(key, judgment);
                }
                into.push(None);
                continue;
            }
            Made::Built(key, made) => (key, made),
        };
        if let (Some(memo), Some(_)) = (&memo, &key) {
            memo.tally(false);
        }
        let kept = kept.unwrap_or_default();
        let refused_before = r.kernel.len();
        let top = match made {
            Ok(b) => b,
            Err(g) => {
                refuse_gap(g, j.module(), j.owner(), &mut r);
                remember(memo.as_ref(), key, kept, &r.kernel[refused_before..]);
                into.push(None);
                continue;
            }
        };
        // `VYRN_KERNEL_TRACE=<fn>` prints that body's core, lambdas included.
        if std::env::var("VYRN_KERNEL_TRACE").is_ok_and(|v| v != "1" && top.name.contains(&v)) {
            eprintln!("{}", top.render());
        }
        // A lambda's rows are keyed by its own nodes under the enclosing
        // function's id, where the emitters read them.
        place_frames(
            &top,
            placed,
            j.id(),
            own,
            &mut added,
            &mut touched,
            &mut r.kernel,
            trace,
        );
        let refused = typed(program, own, &mut r, &top, j.module(), j.as_written());
        let key = key.filter(|_| !refused);
        remember(memo.as_ref(), key, kept, &r.kernel[refused_before..]);
        into.push(Some(top));
    }
    // Every generic function is built once more with its parameters as
    // written, the way the checker typed it, for the judgment alone. It
    // places no row and is never emitted.
    let written = Ownership {
        proto: own.proto.as_written(),
        ..own.clone()
    };
    for inst in crate::as_written(program, own) {
        match build_in(program, &inst, &written, &w.fns, &mut names) {
            Ok(top) => {
                typed(program, own, &mut r, &top, &inst.func.module, true);
                for body in top.frames() {
                    if let Err(rs) = crate::kernel::placement(body, &own.state_callees) {
                        r.kernel.extend(rs);
                    }
                }
            }
            Err(g) => refuse_gap(g, &inst.func.module, &inst.func.name, &mut r),
        }
    }
    // Each `impl` projection's body, built before the judgment.
    for (p, top) in &places {
        match top {
            Ok(top) => {
                crate::world::add_callees(top, &by_name, calls.entry(p.id).or_default());
                typed(program, own, &mut r, top, &p.func.module, true);
            }
            Err(g) => refuse_gap(g.clone(), &p.func.module, &p.func.name, &mut r),
        }
    }
    // Each module-state initializer and each `where` predicate, for the
    // judgment alone. An initializer the checker did not type has no core;
    // the checker's refusal is its sentence.
    for (i, g) in (0..).zip(&program.globals) {
        if node_ty(own, g.init.id()).is_none() {
            continue;
        }
        let source = Source::Expr {
            facts: &lowered.globals,
            file: g.module.clone(),
            binds: None,
            e: &g.init,
        };
        match build_from(program, own, &w.fns, &mut names, &source) {
            Ok(top) => {
                let at = program.source_id(SourceBody::Global(i));
                crate::world::add_callees(&top, &by_name, calls.entry(at).or_default());
                typed(program, own, &mut r, &top, &g.module, true);
            }
            Err(e) => {
                refuse_gap(e, &g.module, &g.name, &mut r);
            }
        }
    }
    for (i, d) in (0..).zip(&program.type_decls) {
        let Some(p) = &d.predicate else { continue };
        let binds: Vec<(String, Type)> = match &d.base {
            Type::Record(fields) => fields
                .iter()
                .map(|f| (f.name.clone(), f.ty.clone()))
                .collect(),
            base => vec![("value".to_string(), base.clone())],
        };
        let source = Source::Expr {
            facts: &lowered.predicates,
            file: d.module.clone(),
            binds: Some(&binds),
            e: p,
        };
        match build_from(program, own, &w.fns, &mut names, &source) {
            Ok(top) => {
                let at = program.source_id(SourceBody::TypeDecl(i));
                crate::world::add_callees(&top, &by_name, calls.entry(at).or_default());
                typed(program, own, &mut r, &top, &d.module, true);
            }
            Err(e) => {
                refuse_gap(e, &d.module, &d.name, &mut r);
            }
        }
    }
    // The lint re-checks the types, which fails two kinds of program by
    // design: a generator host (its generator helpers use `lex`, `render`
    // and `Token`, which an ordinary check types `<type error>`), and one the
    // typed judgment refused (an unknown name typed `<type error>`, never
    // emitted).
    debug_assert!(
        program.host.gen || !r.typed.is_empty() || crate::lint(&lowered).is_empty(),
        "the lowered form failed its own lint:
  {}",
        crate::lint(&lowered).join(
            "
  "
        )
    );
    // A placed release of a generic declared release is a call the lowering's
    // worklist follows ([`crate::dispatched`]) only once the row is in the
    // plan, so a program whose rows name an instance it does not hold is
    // lowered again below. The instances it holds had their callees followed.
    let mut had: std::collections::HashSet<String> =
        lowered.instances.iter().map(Instance::spelling).collect();
    let placed: Vec<Release> = added.values().flatten().cloned().collect();
    let mut dispatches = crate::dispatches_new(&placed, &by_name, &had);
    w.late = dispatches;
    for (f, rows) in added {
        touched.insert(f);
        own.releases.entry(f).or_default().extend(rows);
    }
    // A second build for the emitters, after every row the placer added: the
    // core above read the plan before this pass filled it. A host that armed
    // the memo runs no emitter, and served bodies would leave the facts
    // partial, so it stops here.
    if memo.is_some() {
        // The editor reads what the document's own functions cost
        // ([`crate::insight::fn_costs`]). The memo never serves the root file's bodies, so
        // each is built, and these are the only bodies of this analysis that are kept.
        let mut unused = Facts::default();
        for (inst, top) in lowered.instances.iter().zip(&mut built) {
            if let (None, Some(top)) = (&inst.func.module, top) {
                fold_frames(program, top, own, &mut unused, &mut w.fns, &mut w.bodies);
            }
        }
        w.calls.replace(calls);
        own.state_callees.clear();
        own.accumulators.clear();
        (w.refusals, w.typed) = (r.kernel, r.typed);
        return;
    }
    let _p2 = vyrn_frontend::prof::phase("placer: facts rebuild");
    let mut facts = Facts::default();
    // `vyrn check` emits nothing, so its refusal analysis folds no facts; the
    // worklist below still places its rows. A generator compiled during the
    // load still needs its facts.
    let folds = !judging || vyrn_frontend::movecheck::emitting();
    if folds {
        let state = build_module_state(program, own, &w.fns, &lowered.globals);
        let mut tops: Vec<Body> = state.into_iter().collect();
        // Rebuilt only where the pass above wrote a row for the function; the
        // rest fold the body that pass already built. The rebuilds read `own`
        // and `w.fns` and write nothing, so they run on every thread; the
        // merge is in job order, as the first builds'.
        let shared: &Ownership = own;
        let fns = &w.fns;
        let jobs: Vec<Job> = (lowered.instances.iter().map(Job::Inst))
            .chain(lowered.bodies.iter().map(Job::Outside))
            .collect();
        let _p = vyrn_frontend::prof::phase("placer: facts: rebuilt");
        let sealed = program.expansions.seal();
        let fresh = vyrn_frontend::par::in_parallel(
            &jobs,
            |j| {
                if touched.contains(&j.id()) {
                    j.weight()
                } else {
                    0
                }
            },
            NameMemo::default,
            |names, j| {
                if !touched.contains(&j.id()) {
                    return None;
                }
                match j {
                    Job::Inst(inst) => build_in(program, inst, shared, fns, names).ok(),
                    Job::Outside(ob) => build_outside(program, shared, fns, names, ob).ok(),
                }
            },
        );
        drop((sealed, _p));
        // `test` and `bench` bodies follow the instances, whose nodes an emitter looks up too.
        let mut fresh = fresh.into_iter();
        for b in built.iter_mut().chain(outside.iter_mut()) {
            tops.extend(fresh.next().flatten().or(b.take()));
        }
        for mut top in tops {
            fold_frames(
                program,
                &mut top,
                own,
                &mut facts,
                &mut w.fns,
                &mut w.bodies,
            );
        }
    }
    // A worklist to a fixpoint. Each body is built, placed, and built again,
    // because the first build read the plan before the rows its own placement
    // adds; a row that reaches a generic declared release turns it again.
    // Measure: the instances not yet built, a finite set (the declared
    // releases times the types the program instantiates). A round turns only
    // after one that built at least one of them, since only a new body's
    // placement adds to `placed`.
    while dispatches {
        let again = crate::lower_with(program, own);
        let mut placed: Vec<Release> = Vec::new();
        for inst in &again.instances {
            if !had.insert(inst.spelling()) {
                continue;
            }
            w.fns.instance(inst);
            if let Ok(mut top) = build_in(program, inst, own, &w.fns, &mut names) {
                w.fns.number(&mut top);
                crate::world::add_callees(&top, &by_name, calls.entry(inst.func_id).or_default());
                let mut rows = Added::new();
                let frames = placements(&top, &own.state_callees);
                place_frames(
                    &top,
                    frames,
                    inst.func_id,
                    own,
                    &mut rows,
                    &mut touched,
                    &mut r.kernel,
                    trace,
                );
                for (f, rows) in rows {
                    placed.extend(rows.iter().cloned());
                    own.releases.entry(f).or_default().extend(rows);
                }
            }
            if !folds {
                continue;
            }
            let Ok(mut top) = build_in(program, inst, own, &w.fns, &mut names) else {
                continue;
            };
            fold_frames(
                program,
                &mut top,
                own,
                &mut facts,
                &mut w.fns,
                &mut w.bodies,
            );
        }
        dispatches = crate::dispatches_new(&placed, &by_name, &had);
    }
    w.calls.replace(calls);
    w.facts = folds.then_some(facts);
    (w.refusals, w.typed) = (r.kernel, r.typed);
    own.state_callees.clear();
    own.accumulators.clear();
}

/// One body `augment` builds: an instance, or a `test` or `bench` body.
#[derive(Clone, Copy)]
enum Job<'l, 'p> {
    Inst(&'l Instance<'p>),
    Outside(&'l crate::OutsideBody<'p>),
}

impl Job<'_, '_> {
    /// The row the plan's tables and the call relation key the body by.
    fn id(&self) -> FnId {
        match self {
            Job::Inst(inst) => inst.func_id,
            Job::Outside(ob) => ob.id,
        }
    }

    /// The name the effect judgment and a gap's refusal name the body by.
    fn owner(&self) -> &str {
        match self {
            Job::Inst(inst) => &inst.func.name,
            Job::Outside(ob) => &ob.name,
        }
    }

    fn module(&self) -> &Option<String> {
        match self {
            Job::Inst(inst) => &inst.func.module,
            Job::Outside(ob) => &ob.module,
        }
    }

    /// Whether the typed judgment reads the body as written: every body but
    /// a generic instance.
    fn as_written(&self) -> bool {
        match self {
            Job::Inst(inst) => inst.subst.is_empty(),
            Job::Outside(_) => true,
        }
    }

    /// The judgment memo's key. A `test` or `bench` body is keyed with its
    /// line too: the `test@<i>` index is global, so a test added to an
    /// earlier module renumbers every later one.
    fn key(&self, memo: &vyrn_frontend::movecheck::Judgments<'_>) -> Option<JudgmentKey> {
        match self {
            Job::Inst(inst) => memo.key(inst.func.module.as_deref(), &inst.spelling()),
            Job::Outside(ob) => memo.key(ob.module.as_deref(), &format!("{}@{}", ob.name, ob.line)),
        }
    }

    /// The expressions the body holds, the measure
    /// [`vyrn_frontend::par::in_parallel`] orders by.
    fn weight(&self) -> usize {
        match self {
            Job::Inst(inst) => inst.facts.exprs.len(),
            Job::Outside(ob) => ob.facts.exprs.len(),
        }
    }

    fn build(
        &self,
        program: &Program,
        own: &Ownership,
        fns: &Fns,
        names: &mut NameMemo,
    ) -> Result<Body, Gap> {
        match self {
            Job::Inst(inst) => {
                let _p = vyrn_frontend::prof::phase("placer: core::build");
                let inst = crate::walked(program, &own.record, inst);
                build_in(program, &inst, own, fns, names)
            }
            Job::Outside(ob) => {
                let _p = vyrn_frontend::prof::phase("placer: build_outside");
                build_outside(program, own, fns, names, ob)
            }
        }
    }
}

/// One body `augment` has not yet built or served: its memo key, and the
/// memo's answer for it.
struct Pending<'l, 'p> {
    job: Job<'l, 'p>,
    key: Option<JudgmentKey>,
    served: Option<(JudgmentKey, Judgment)>,
}

/// One body `augment` works on, in job order: the instances, then the `test`
/// and `bench` bodies.
struct JobState<'l, 'p> {
    job: Job<'l, 'p>,
    made: Made,
    /// What the memo keeps of the body ([`Kept`]), when it was judged.
    kept: Option<Kept>,
}

impl JobState<'_, '_> {
    /// The body's owner and core, when it built.
    fn built(&self) -> Option<(&str, &Body)> {
        match &self.made {
            Made::Built(_, Ok(b)) => Some((self.job.owner(), b)),
            _ => None,
        }
    }

    /// The body's owner and the frames the memo served, when it served any.
    fn answered(&self) -> Option<(&str, &[Walked])> {
        match &self.made {
            Made::Served(_, s) if !s.frames.is_empty() => Some((self.job.owner(), &s.frames[..])),
            _ => None,
        }
    }
}

/// One body `augment` built, or served out of the memo.
enum Made {
    /// The memo's entry for it, taken out until `augment` puts it back.
    Served(JudgmentKey, Judgment),
    Built(Option<JudgmentKey>, Result<Body, Gap>),
}

/// What the memo keeps of a built body besides its refusals: its frames as
/// the effect judgment read them, and the judgment's answer for each.
type Kept = (Vec<Walked>, Vec<Vec<(String, Vec<String>)>>);

/// One body's entry out of the judgment memo, with its key. `Some` means the
/// body is neither built nor judged, unless the effect judgment answers its
/// frames otherwise than when it was recorded.
fn serve(
    memo: Option<&vyrn_frontend::movecheck::Judgments<'_>>,
    key: Option<&JudgmentKey>,
) -> Option<(JudgmentKey, Judgment)> {
    let key = key?;
    Some((key.clone(), memo?.take(key)?))
}

/// Records one body's refusals, `refused`, for every body with a key. Serving skips placement too, which a host that
/// armed the memo does not read ([`movecheck::Judgments`]).
fn remember(
    memo: Option<&vyrn_frontend::movecheck::Judgments<'_>>,
    key: Option<JudgmentKey>,
    (frames, state): Kept,
    refused: &[Refusal],
) {
    let (Some(memo), Some(key)) = (memo, key) else {
        return;
    };
    let verdict = refused.to_vec();
    memo.put(
        key,
        Judgment {
            verdict,
            frames,
            state,
        },
    );
}

/// The memory report for one frame, read by the editor's memory hints: one row per source `let`, in line order. Every word
/// comes off the core: the type table says how the type is released, the
/// `let` whose the value is ([`NameInfo::not_owned`]), and the kernel what
/// took it and where the release stands.
fn report(
    body: &Body,
    owner: FnId,
    missing: &[crate::kernel::Missing],
    took: &[Option<crate::kernel::Took>],
    released: &[Option<Vec<String>>],
    own: &mut Ownership,
) {
    // The releases the kernel found owed, and the holes each walks around:
    // "reclaimed at block exit". A whole-value row counts whichever table
    // `place_frames` files it under (exit, edge, arm binder); a returned
    // `match` holds a value at one edge row per arm.
    let mut exits: HashMap<Name, Vec<String>> = HashMap::new();
    for m in missing {
        match m.kind {
            crate::kernel::MissingKind::Exit
            | crate::kernel::MissingKind::Edge { .. }
            | crate::kernel::MissingKind::ArmBinder { .. } => {
                exits.entry(m.name).or_insert_with(|| plan_holes(&m.holes));
            }
            // A sub-place row says nothing about the binding as a whole, and
            // a store row names a place that may be nobody's binding.
            crate::kernel::MissingKind::EdgePlace { .. } | crate::kernel::MissingKind::Store => {}
        }
    }
    // Taken out and put back at the end, so `own` (whose type table holds
    // every declaration) is not copied per frame, which is per keystroke.
    let mut rows = std::mem::take(own.memory.entry(owner).or_default());
    let sp = body.speech();
    for (i, info) in body.names.iter().enumerate() {
        if !info.bound_by_let {
            continue;
        }
        // The first instance of a generic function answers for the source
        // `let`.
        if rows
            .iter()
            .any(|r| r.name == info.source && r.line == info.line)
        {
            continue;
        }
        let name = info.source.clone();
        let line = info.line;
        let took = took.get(i).and_then(|t| t.as_ref());
        let leaked = |text: String, reason: &'static str, heap: bool| MemoryRow {
            name: name.clone(),
            line,
            text,
            last_use: None,
            moved_into: None,
            bucket: Bucket::Leaked { reason, heap },
        };
        let row = match (&info.not_owned, took) {
            (Some(NotOwned::NoRelease { heap: false }), _) => leaked(
                format!("NOT reclaimed — the type {} owns no heap", sp.ty(&info.ty)),
                "the type owns no heap",
                false,
            ),
            (Some(NotOwned::NoRelease { heap: true }), _) => leaked(
                format!(
                    "NOT reclaimed — nothing releases the type {} yet",
                    sp.ty(&info.ty)
                ),
                "the type has no release rule",
                true,
            ),
            (Some(NotOwned::Borrow(what)), _) => leaked(
                format!("NOT reclaimed — it is {what}"),
                "it names somebody else's value",
                true,
            ),
            (Some(NotOwned::Static), _) => MemoryRow {
                name,
                line,
                text: "static data — nothing reclaims it, and nothing needs to".to_string(),
                last_use: None,
                moved_into: None,
                bucket: Bucket::Static,
            },
            // A must-use value handed to a builtin is disposed of, not moved.
            (Some(NotOwned::MustUse(l)), Some(t)) if t.builtin => MemoryRow {
                name,
                line,
                text: discharged(l, &sp),
                last_use: None,
                moved_into: None,
                bucket: Bucket::Discharged,
            },
            (_, Some(t)) if t.how == crate::kernel::TookHow::Drop => MemoryRow {
                name,
                line,
                text: format!("reclaimed by `drop` at line {}", t.line),
                last_use: Some(t.line),
                moved_into: None,
                bucket: Bucket::Dropped,
            },
            // A `return` is named "the return": it has no taker to look at.
            (_, Some(t)) => {
                let into = match t.how {
                    crate::kernel::TookHow::Return => "the return".to_string(),
                    _ => t.by.to_string(),
                };
                MemoryRow {
                    name,
                    line,
                    text: format!("moved at line {} into {into}", t.line),
                    last_use: Some(t.line),
                    moved_into: Some(into),
                    bucket: Bucket::Moved,
                }
            }
            // A program that reaches here discharges every must-use value,
            // and the discharging construct frees it.
            (Some(NotOwned::MustUse(l)), None) => MemoryRow {
                name,
                line,
                text: discharged(l, &sp),
                last_use: None,
                moved_into: None,
                bucket: Bucket::Discharged,
            },
            (None, None) => match (
                own.proto.release_kind(&info.ty),
                exits
                    .get(&Name(i as u32))
                    .cloned()
                    .or_else(|| released[i].as_deref().map(plan_holes)),
            ) {
                (Some(kind), Some(holes)) => MemoryRow {
                    name,
                    line,
                    text: reclaimed(&kind, &holes, &sp),
                    last_use: None,
                    moved_into: None,
                    bucket: Bucket::Reclaimed,
                },
                // Owned, and no release placed; the core states no reason.
                _ => leaked(
                    "NOT reclaimed — nothing in this frame releases it".to_string(),
                    "nothing releases it here",
                    info.heap,
                ),
            },
        };
        rows.push(row);
    }
    rows.sort_by_key(|r| r.line);
    own.memory.insert(owner, rows);
}

/// The "reclaimed at block exit" sentence, with the places a `consume` took
/// out of the value, which the release walks around.
fn reclaimed(kind: &DropKind, holes: &[String], sp: &Speech) -> String {
    if holes.is_empty() {
        return format!("reclaimed at block exit — {}", kind.words(sp));
    }
    let places = holes
        .iter()
        .map(|p| format!("`{p}`"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "reclaimed at block exit — {}, except {places}, which a `consume` took",
        kind.words(sp)
    )
}

/// The must-use sentence: what the program wrote to discharge the value, and
/// which lowering frees it.
fn discharged(l: &Linear, sp: &Speech) -> String {
    match l {
        Linear::Stream => "discharged, not leaked — a stream is consumed, forwarded or closed \
             on every path, and that lowering frees it"
            .to_string(),
        Linear::Declared(by) => format!(
            "discharged, not leaked — `{}` declares `impl MustUse`, so it is handed on or \
             dropped on every path",
            sp.name(by)
        ),
    }
}

/// The rows `augment` places, by owner, each owner's in placement order. No
/// reader depends on the order of owners.
type Added = HashMap<FnId, Vec<Release>>;

/// The kernel's placement of each frame of `top`, in frame order.
fn placements(
    top: &Body,
    state: &vyrn_frontend::own::StateCallees,
) -> Vec<Result<crate::kernel::Placement, Vec<Refusal>>> {
    let run = |body: &Body| {
        let _k = vyrn_frontend::prof::phase("placer: kernel::placement");
        crate::kernel::placement(body, state)
    };
    top.frames().into_iter().map(run).collect()
}

/// Places what one built body owes, frame by frame, from `placed`, its
/// [`placements`]. `owner` keys the plan's tables: the function, or the
/// `test` or `bench` body. A lambda frame is keyed by its enclosing body.
#[allow(clippy::too_many_arguments)]
fn place_frames(
    top: &Body,
    placed: Vec<Result<crate::kernel::Placement, Vec<Refusal>>>,
    owner: FnId,
    own: &mut Ownership,
    added: &mut Added,
    touched: &mut std::collections::HashSet<FnId>,
    refusals: &mut Vec<Refusal>,
    trace: bool,
) {
    for (body, placed) in top.frames().into_iter().zip(placed) {
        let crate::kernel::Placement {
            missing,
            took,
            released,
        } = match placed {
            Ok(m) => m,
            Err(rs) => {
                for r in rs {
                    if trace {
                        eprintln!("placer: refused: {}: {}", r.body, r.diagnostic.message);
                    }
                    // No placement repairs these. Every one the body earns is
                    // kept, so the driver can merge by binding and line.
                    refusals.push(r);
                }
                continue;
            }
        };
        let rp = vyrn_frontend::prof::phase("placer: report");
        report(body, owner, &missing, &took, &released, own);
        drop(rp);
        for m in missing {
            // A store's row is keyed by the store alone: its place may be no
            // binding of this frame, so it is handled before the name is read.
            if m.kind == MissingKind::Store {
                let fresh = (own.placed.stores)
                    .insert(m.site, m.holes.clone())
                    .is_none();
                if fresh {
                    if trace {
                        eprintln!("placer: {} store at {:?} releases", body.name, m.site);
                    }
                    touched.insert(owner);
                }
                continue;
            }
            let info = &body.names[m.name.index()];
            let kind = own.proto.release_kind(&info.ty);
            if trace {
                eprintln!(
                    "placer: {} `{}` (line {}) {:?} at {:?} site {:?} kind {:?} holes {:?}",
                    body.name, info.source, info.line, m.kind, m.exit, m.site, kind, m.holes
                );
            }
            // A receiver a consumer borrowed out of ([`NameInfo::producer`]):
            // an argument temporary keyed by the producing node.
            if let Some(producer) = info.producer {
                let fresh = own.placed.producers.insert(producer);
                if fresh {
                    touched.insert(owner);
                }
                continue;
            }
            if m.site == NodeId::NONE {
                continue;
            }
            let Some(kind) = kind else {
                continue;
            };
            // An element hole (`.[]`) cannot be skipped: no row, and the
            // judgment refuses the name.
            if m.holes.iter().any(|h| h.contains("[]")) {
                continue;
            }
            let holes: Vec<String> = m
                .holes
                .iter()
                .map(|h| h.trim_start_matches('.').to_string())
                .collect();
            // A declared release takes the whole value: it cannot
            // be told a hole.
            if !holes.is_empty() && matches!(kind, DropKind::Release(..)) {
                continue;
            }
            match m.kind {
                // Rule N: one edge of a join still holds what another took.
                // Keyed by name, so a loop variable qualifies.
                MissingKind::Edge { edge } => {
                    let rows = own.placed.edges.entry(m.site).or_default();
                    if !rows.iter().any(|(n, e, _)| *n == info.source && *e == edge) {
                        rows.push((info.source.clone(), edge, holes));
                        touched.insert(owner);
                    }
                    continue;
                }
                // The sub-place one edge took, released on the other, spelled
                // `d.line`.
                MissingKind::EdgePlace { edge, path } => {
                    let name = format!("{}{}", info.source, path);
                    let rows = own.placed.edges.entry(m.site).or_default();
                    if !rows.iter().any(|(n, e, _)| *n == name && *e == edge) {
                        rows.push((name, edge, Vec::new()));
                        touched.insert(owner);
                    }
                    continue;
                }
                // The arm's unmoved payload binders, one entry each, with the
                // holes the arm left.
                MissingKind::ArmBinder { arm } => {
                    let rows = own.placed.arms.entry((m.site, arm)).or_default();
                    if !rows.iter().any(|(n, _)| *n == info.source) {
                        rows.push((info.source.clone(), holes));
                        touched.insert(owner);
                    }
                    continue;
                }
                MissingKind::Exit => {}
                MissingKind::Store => unreachable!("read above, keyed by the store"),
            }
            // A field read's unnamed receiver is stated on the name
            // (`NameInfo::receiver`, `NameInfo::holes`), not as a row.
            if info.receiver.is_some() {
                continue;
            }
            // An owed release of a temporary has no row to key: the builder
            // states it (`Builder::discards`, `Builder::drop_receiver`,
            // `Builder::drop_since`, `Builder::leave_try`), so one found here
            // is a defect.
            let Some(binding) = info.binding else {
                panic!(
                    "placer: `{}` (line {}) in `{}` owes a release at {:?} and has no binding",
                    info.source, info.line, body.name, m.exit
                );
            };
            // A row the plan already placed here takes the kernel's hole set,
            // which is per path where the plan's is per binding.
            if let Some(r) = own.releases.get_mut(&owner).and_then(|rows| {
                rows.iter_mut()
                    .find(|r| r.exit == m.exit && r.site == m.site && r.binding == binding)
            }) {
                if trace {
                    eprintln!(
                        "placer: rewrite {} `{}` {:?} -> {:?}",
                        body.name, info.source, m.exit, holes
                    );
                }
                r.holes = Some(holes);
                touched.insert(owner);
                continue;
            }
            let added = added.entry(owner).or_default();
            if added
                .iter()
                .any(|r| r.exit == m.exit && r.site == m.site && r.binding == binding)
            {
                continue;
            }
            added.push(Release {
                site: m.site,
                binding,
                name: info.source.clone(),
                kind: kind.clone(),
                exit: m.exit,
                line: info.line as u32,
                // The kernel's set at this exit, even when empty: `None`
                // would fall back to the binding's set, which is not per
                // path (`regexredux`'s early `Err` returns walk the whole
                // record).
                holes: Some(holes),
            });
        }
    }
}
