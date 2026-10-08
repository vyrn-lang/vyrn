//! Proves check rows ([`crate::check`]) that cannot fail, from what one body
//! states about its own names ([`crate::facts`]).
//!
//! [`decide`] walks a body's rows forward. A `let` or a store of an integer
//! defines its name; a comparison remembers the facts its truth and falsehood
//! give, and an `if` on it assumes them; a builtin's row states how it moves
//! its receiver's length, and any other `modify` or `consume` argument that is
//! not a scalar forgets its name. After a check row the path has its guard, since
//! it traps otherwise. A loop's head keeps the candidate facts that hold at
//! entry and after every turn (Houdini): each round drops at least one
//! candidate or stops, so the candidate count bounds the rounds.
//!
//! The walk knows nothing about a global, a field or an element: each read of
//! one is a fresh value with its type's range. An exact sum is an `Int64` sum
//! that provably stays in `-2^62..=2^62`. A row is proved only with a
//! certificate that [`crate::facts::Cert::verify`] accepts.
//!
//! One postulate: a live read borrow's source is not written, by the kernel's
//! exclusivity judgment, so a borrow's length changes only where the walk sees
//! the borrow itself written.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Write;

use vyrn_frontend::ast::{BinOp, Capability, FnId, Type, TypeDecl, UnOp};
use vyrn_frontend::effects::Effect;
use vyrn_frontend::prelude::{self, Length};
use vyrn_frontend::prim::Cmp;

use crate::facts::{Lin, State, Term};
use vyrn_frontend::core::check::{Check, Guard, Site, Verdict, Why};
use vyrn_frontend::core::{Arg, Body, Callee, Ctor, Lit, Name, Op, Place, Rhs, St, Val};

/// The bound an exact `Int64` sum must provably stay within: `2^62`, so no
/// premise or goal of the prover itself leaves `i64`.
const EXACT: i64 = 1 << 62;

/// Marks every check row of `body`, and of each lambda body it holds, that
/// cannot fail [`Verdict::Proved`].
pub fn decide(body: &mut Body, decls: &HashMap<String, TypeDecl>) {
    for l in &mut body.lambdas {
        decide(l, decls);
    }
    refuted(body, decls);
}

/// A group's rule check that fails wherever it runs: its site, the field the
/// facts prove longer, and the field they prove shorter, as the rule names
/// them.
pub type Refuted = (Site, String, String);

/// Decides the check rows of the one frame `body` as [`decide`] does, without
/// its lambdas. Answers each rule check that ends a group
/// ([`crate::typed::groups`]) where the facts prove one field of an equal
/// pair longer than the other, on every live path. A dead state proves every
/// goal, so it refutes none.
pub fn refuted(body: &mut Body, decls: &HashMap<String, TypeDecl>) -> Vec<Refuted> {
    if !any_check(&body.stmts) {
        return Vec::new();
    }
    let mut stmts = std::mem::take(&mut body.stmts);
    let mut w = Walk {
        body,
        decls,
        loops: Vec::new(),
        record: true,
        memo: HashMap::new(),
        slot: relevant(body, &stmts),
        open: BTreeSet::new(),
        refuted: Vec::new(),
    };
    let mut st = State::default();
    for p in &body.params {
        w.slot[p.index()].origin = Origin::Param(*p);
        w.fresh(&mut st, *p);
    }
    w.block(st, &mut stmts);
    let refuted = w.refuted;
    body.stmts = stmts;
    refuted
}

fn any_check(ss: &[St]) -> bool {
    ss.iter().any(|s| match s {
        St::Check(_) => true,
        St::If { then, els, .. } => any_check(then) || any_check(els),
        St::Loop { body, .. } | St::Block { body, .. } => any_check(body),
        St::Switch { arms, .. } => arms.iter().any(|a| any_check(&a.body)),
        _ => false,
    })
}

struct Walk<'a> {
    body: &'a Body,
    decls: &'a HashMap<String, TypeDecl>,
    /// Per enclosing loop, innermost last: the states at its `break`s and at
    /// its `continue`s.
    loops: Vec<Loop>,
    /// Whether a check row's verdict is written: false in Houdini's rounds,
    /// true in the replay from the settled head.
    record: bool,
    /// A loop's exit state by its rows' address and its entry state, for the
    /// walks that write no verdict.
    memo: HashMap<(usize, State), State>,
    /// Per name, what the walk keeps about it ([`relevant`]).
    slot: Vec<Slot>,
    /// The records a store into a field has left unchecked: from the store to
    /// the record's rule check ([`Guard::Rule`]), each field's length is a
    /// term of its own ([`Walk::col`]).
    open: BTreeSet<Name>,
    refuted: Vec<Refuted>,
}

/// What the walk keeps about one name.
#[derive(Clone, Copy)]
struct Slot {
    /// Whether a check's goal can depend on the name; the walk states nothing
    /// about any other name.
    relevant: bool,
    /// Where it got its value, for a kept check's [`Why`].
    origin: Origin,
}

/// Where a name got its value, weakest first. A name holds the strongest
/// origin of its bindings, the first of equal strength; each variant names
/// the binding that decided.
#[derive(Clone, Copy)]
enum Origin {
    /// A literal, or an operator over such.
    Local,
    Param(Name),
    /// A field, element, map or global read, or a payload.
    Place(Name),
    /// A call's result: the name it binds, and the function when it is a
    /// declared one.
    Call(Name, Option<FnId>),
    /// A builtin that reads the outside world.
    Input(Name),
}

impl Origin {
    fn rank(self) -> u8 {
        match self {
            Origin::Local => 0,
            Origin::Param(_) => 1,
            Origin::Place(_) => 2,
            Origin::Call(..) => 3,
            Origin::Input(_) => 4,
        }
    }

    fn join(self, o: Origin) -> Origin {
        if o.rank() > self.rank() {
            o
        } else {
            self
        }
    }
}

/// One enclosing loop of the walk.
struct Loop {
    /// The states at its `break`s and at its `continue`s, in the round
    /// being walked.
    breaks: Vec<State>,
    conts: Vec<State>,
    seen: Seen,
}

/// What the rows of one loop did to its names, over every round of the walk.
struct Seen {
    /// Every name the loop writes ([`writes`]).
    written: BTreeSet<Name>,
    /// The names it stores to: its counters ([`stores`]).
    stored: BTreeSet<Name>,
    /// Names a call took by `modify` or `consume`, which forgot their length.
    resized: BTreeSet<Name>,
}

/// What a primitive row states about its result.
enum Out {
    Def(Lin),
    /// The facts the result's truth gives, and those its falsehood gives.
    Cond(Vec<Lin>, Vec<Lin>),
    Facts(Vec<Lin>),
    Nothing,
}

enum Kind {
    /// An integer of this many bits, signed or not.
    Int(u8, bool),
    /// An array or a String: it has a length.
    Seq,
    Other,
}

impl Walk<'_> {
    fn kind(&self, n: Name) -> Kind {
        match &*vyrn_frontend::types::resolved(&self.body.names[n.index()].ty, self.decls) {
            t if t.is_seq() || *t == Type::Str => Kind::Seq,
            t => match vyrn_frontend::validate::width(t) {
                Some((bits, signed)) => Kind::Int(bits, signed),
                None => Kind::Other,
            },
        }
    }

    /// The length of `p`: an array or String name, or such a field of a record
    /// name ([`Walk::col`]).
    fn length(&self, p: &Place) -> Option<Term> {
        match p {
            Place::Name(b) if matches!(self.kind(*b), Kind::Seq) => Some(Term::Len(*b)),
            Place::Field(r, f) => {
                let Place::Name(r) = &**r else { return None };
                let (own, least) = self.col(*r, f)?;
                let at = if self.open.contains(r) { own } else { least };
                Some(Term::Col(*r, at))
            }
            _ => None,
        }
    }

    /// The array or String field `f` of record name `r`, by its own index and
    /// by the least index among the fields the record's `where` rule states of
    /// equal length. The rule holds wherever the record is checked, so one
    /// term is the length of every field of a class ([`Term::Col`]).
    fn col(&self, r: Name, f: &str) -> Option<(u32, u32)> {
        let seq = |t: &Type| {
            let t = vyrn_frontend::types::resolve(t, self.decls);
            t.is_seq() || t == Type::Str
        };
        let ty = &self.body.names[r.index()].ty;
        let fields = vyrn_frontend::types::record_fields(ty, self.decls)?;
        let at = |g: &str| fields.iter().position(|x| x.name == g && seq(&x.ty));
        let own = at(f)?;
        let mut class = BTreeSet::from([own]);
        let pairs = self
            .rule(r)
            .map(vyrn_frontend::types::predicate_equal_lengths)
            .unwrap_or_default();
        // Each round adds a field or stops.
        loop {
            let before = class.len();
            for (a, b) in &pairs {
                if let (Some(a), Some(b)) = (at(a), at(b)) {
                    if class.contains(&a) || class.contains(&b) {
                        class.extend([a, b]);
                    }
                }
            }
            if class.len() == before {
                break;
            }
        }
        let least = *class.first().expect("holds the field itself");
        Some((u32::try_from(own).ok()?, u32::try_from(least).ok()?))
    }

    fn rule(&self, r: Name) -> Option<&vyrn_frontend::ast::Expr> {
        match &self.body.names[r.index()].ty {
            Type::Named(n) => self.decls.get(n)?.predicate.as_ref(),
            _ => None,
        }
    }

    /// Tracks the fields of `r` apart from a row that may change the length
    /// of one ([`Walk::open`]). The rule held until then, so each field's own
    /// term starts equal to its class's.
    fn open(&mut self, st: &mut State, p: &Place) {
        let Place::Field(r, _) = p else { return };
        let Place::Name(r) = &**r else { return };
        if self.rule(*r).is_none() || !self.open.insert(*r) {
            return;
        }
        let ty = &self.body.names[r.index()].ty;
        let fields = vyrn_frontend::types::record_fields(ty, self.decls).unwrap_or_default();
        for f in fields.iter() {
            if let Some((own, least)) = self.col(*r, &f.name).filter(|(o, l)| o != l) {
                let t = Term::Col(*r, own);
                st.forget(t);
                st.assume_eq(&Lin::of(t), &Lin::of(Term::Col(*r, least)));
            }
        }
    }

    fn is_int64(&self, n: Name) -> bool {
        matches!(self.kind(n), Kind::Int(64, true))
    }

    /// `n` as a fresh value: its type's range, and a fixed-size array's length.
    fn fresh(&self, st: &mut State, n: Name) {
        st.kill(n);
        self.range(st, n);
    }

    /// What `n`'s type says about it, unless a definition says more.
    fn range(&self, st: &mut State, n: Name) {
        if !self.slot[n.index()].relevant
            || st.defs.contains_key(&Term::Val(n))
            || st.defs.contains_key(&Term::Len(n))
        {
            return;
        }
        match self.kind(n) {
            Kind::Int(bits, signed) if bits < 64 => {
                let (lo, hi) = if signed {
                    (-(1i64 << (bits - 1)), (1i64 << (bits - 1)) - 1)
                } else {
                    (0, (1i64 << bits) - 1)
                };
                let v = Lin::of(Term::Val(n));
                st.assume(&v.plus(-lo).expect("a narrow range fits"));
                st.assume(&Lin::k(hi).sub(&v).expect("a narrow range fits"));
            }
            Kind::Int(64, false) => st.assume(&Lin::of(Term::Val(n))),
            Kind::Seq => {
                let ty = vyrn_frontend::types::resolve(&self.body.names[n.index()].ty, self.decls);
                if let Type::ArrayN(_, len) = ty {
                    st.assume_eq(&Lin::of(Term::Len(n)), &Lin::k(len as i64));
                }
            }
            _ => {}
        }
    }

    /// The exact value of `v`, if it is an integer.
    fn lin(&self, v: &Val) -> Option<Lin> {
        match v {
            Val::Lit(Lit::Int(k)) => Some(Lin::k(*k)),
            Val::Lit(Lit::Byte(b)) => Some(Lin::k(i64::from(*b))),
            Val::Name(n) if matches!(self.kind(*n), Kind::Int(..)) => Some(Lin::of(Term::Val(*n))),
            _ => None,
        }
    }

    /// `r`, when the state proves it stays in `-EXACT..=EXACT`.
    fn fits(st: &State, r: Option<Lin>) -> Option<Lin> {
        let r = r?;
        let hi = Lin::k(EXACT).sub(&r)?;
        let lo = r.plus(EXACT)?;
        (st.ge0(&hi).is_some() && st.ge0(&lo).is_some()).then_some(r)
    }

    fn block(&mut self, mut st: State, ss: &mut [St]) -> State {
        for s in ss {
            if st.dead {
                // A check in code the walk takes for dead is proved: a dead
                // state proves every goal.
                self.dead(s);
                continue;
            }
            st = self.stmt(st, s);
        }
        st
    }

    fn dead(&mut self, s: &mut St) {
        match s {
            St::Check(c) if self.record && provable(&c.guard) => c.verdict = Verdict::Proved,
            St::If { then, els, .. } => {
                then.iter_mut().for_each(|s| self.dead(s));
                els.iter_mut().for_each(|s| self.dead(s));
            }
            St::Loop { body, .. } | St::Block { body, .. } => {
                body.iter_mut().for_each(|s| self.dead(s))
            }
            St::Switch { arms, .. } => {
                for a in arms {
                    a.body.iter_mut().for_each(|s| self.dead(s));
                }
            }
            _ => {}
        }
    }

    fn stmt(&mut self, mut st: State, s: &mut St) -> State {
        match s {
            St::Let(n, rhs) => {
                self.bind(&mut st, *n, rhs);
                st
            }
            St::Do { rhs, .. } => {
                self.effects(&mut st, rhs);
                // The rule holds again after its check.
                if let Some(r) = rhs.checks_rule(&self.body.names) {
                    self.open.remove(&r);
                }
                st
            }
            St::Store { place, value, .. } => {
                if let Place::Name(n) = place {
                    let o = self.slot[n.index()].origin.join(self.origin_of_val(value));
                    self.slot[n.index()].origin = o;
                }
                self.open(&mut st, place);
                match (&*place, self.length(place)) {
                    // The field takes the stored array's length.
                    (Place::Field(..), Some(t)) => {
                        st.forget(t);
                        let stored = match value {
                            Val::Name(v) => self.length(&Place::Name(*v)),
                            Val::Lit(_) => None,
                        };
                        if let Some(l) = stored {
                            st.define(t, &Lin::of(l));
                        }
                    }
                    _ => {
                        if let Some(n) = resized_by_store(place) {
                            st.kill(n);
                        }
                    }
                }
                if let Place::Name(n) = place {
                    if self.slot[n.index()].relevant {
                        self.assign(&mut st, *n, value);
                        self.range(&mut st, *n);
                    }
                }
                st
            }
            St::Drop(n, ..) => {
                st.kill(*n);
                st
            }
            St::Row { .. } => st,
            St::If {
                cond, then, els, ..
            } => {
                let (yes, no) = match cond {
                    Val::Lit(Lit::Bool(true)) => (st.clone(), State::dead()),
                    Val::Lit(Lit::Bool(false)) => (State::dead(), st.clone()),
                    Val::Name(c) => {
                        let (t, f) = st.conds.get(c).cloned().unwrap_or_default();
                        let (mut yes, mut no) = (st.clone(), st.clone());
                        t.iter().for_each(|l| yes.assume(l));
                        f.iter().for_each(|l| no.assume(l));
                        (yes, no)
                    }
                    _ => (st.clone(), st.clone()),
                };
                let a = self.block(yes, then);
                let b = self.block(no, els);
                State::join(&[a, b])
            }
            St::Block { body, .. } => self.block(st, body),
            St::Loop { body, .. } => self.looped(st, body),
            St::Switch { on, arms, .. } => {
                let mut outs = vec![st.clone()];
                for a in arms {
                    let mut s = st.clone();
                    for b in &a.binds {
                        self.slot[b.index()].origin = self.origin_of_val(on);
                        self.fresh(&mut s, *b);
                    }
                    outs.push(self.block(s, &mut a.body));
                }
                State::join(&outs)
            }
            St::Break { .. } => {
                if let Some(l) = self.loops.last_mut() {
                    l.breaks.push(st);
                }
                State::dead()
            }
            St::Continue { .. } => {
                if let Some(l) = self.loops.last_mut() {
                    l.conts.push(st);
                }
                State::dead()
            }
            St::Return { .. } | St::Trap => State::dead(),
            St::Check(c) => {
                let goals = self.goals(&c.guard);
                if self.record {
                    let holds = |g: &Lin| st.ge0(g).is_some_and(|cert| cert.verify(&st, g));
                    let failing = goals.iter().flatten().find(|g| !holds(g));
                    if goals.is_some() && failing.is_none() && provable(&c.guard) {
                        c.verdict = Verdict::Proved;
                    } else {
                        c.why = self.why(&st, c, failing);
                    }
                    if let (Guard::Rule(r), Some(gs), false) = (&c.guard, &goals, st.dead) {
                        // Goals come in pairs, `a - b` then `b - a`, per pair
                        // of the rule; `g < 0` is `-g - 1 >= 0`.
                        let fails = |g: &Lin| {
                            g.scale(-1)
                                .and_then(|n| n.plus(-1))
                                .is_some_and(|n| holds(&n))
                        };
                        let pairs = (self.rule(*r))
                            .map(vyrn_frontend::types::predicate_equal_lengths)
                            .unwrap_or_default();
                        let at = gs.iter().position(fails);
                        if let Some((i, (a, b))) = at.and_then(|i| Some((i, pairs.get(i / 2)?))) {
                            let (long, short) = if i % 2 == 0 { (b, a) } else { (a, b) };
                            self.refuted.push((c.site, long.clone(), short.clone()));
                        }
                    }
                }
                // A passed index, span or shift check states its goals; a
                // divisor's goals are only one way it can pass.
                if matches!(
                    c.guard,
                    Guard::Index(..) | Guard::Span(..) | Guard::Shift(..) | Guard::Rule(..)
                ) {
                    for g in goals.into_iter().flatten() {
                        st.assume(&g);
                    }
                }
                st
            }
        }
    }

    /// What must be `>= 0` for the check to pass, as far as linear facts can
    /// say; `None` when they cannot say it all.
    fn goals(&self, g: &Guard) -> Option<Vec<Lin>> {
        let len = |p: &Place| self.length(p).map(Lin::of);
        Some(match g {
            Guard::Index(p, i) => {
                let i = self.lin(i)?;
                vec![i.clone(), len(p)?.sub(&i)?.plus(-1)?]
            }
            Guard::Span(p, i, n) => {
                let i = self.lin(i)?;
                vec![i.clone(), len(p)?.sub(&i)?.plus(-n)?]
            }
            Guard::Shift(k, bits) => {
                let k = self.lin(k)?;
                vec![k.clone(), Lin::k(i64::from(*bits) - 1).sub(&k)?]
            }
            Guard::NonZero(d) => {
                let d = self.lin(d)?;
                match d.is_const() {
                    true if d.c != 0 => vec![],
                    true => return None,
                    // Only a divisor of one sign proves nonzero linearly.
                    false => vec![d.plus(-1)?],
                }
            }
            Guard::NoOverflow(_, d, _) => vec![self.lin(d)?],
            Guard::Range(..) => return None,
            // Only where each field's length is its own term: elsewhere one
            // term stands for a whole class, and the goal holds by itself.
            Guard::Rule(r) if self.open.contains(r) => {
                let rule = self.rule(*r)?;
                let pairs = vyrn_frontend::types::predicate_equal_lengths(rule);
                if pairs.len() != conjuncts(rule) {
                    return None;
                }
                let mut out = Vec::new();
                for (a, b) in pairs {
                    let own = |f: &str| Some(Lin::of(Term::Col(*r, self.col(*r, f)?.0)));
                    let (a, b) = (own(&a)?, own(&b)?);
                    out.extend([a.sub(&b)?, b.sub(&a)?]);
                }
                out
            }
            Guard::Rule(_) => return None,
        })
    }

    fn bind(&mut self, st: &mut State, n: Name, rhs: &Rhs) {
        let o = self.origin_of(n, rhs);
        self.slot[n.index()].origin = self.slot[n.index()].origin.join(o);
        st.kill(n);
        if !self.slot[n.index()].relevant {
            return self.effects(st, rhs);
        }
        match rhs {
            Rhs::Val(v) => self.assign(st, n, v),
            Rhs::Read(Place::Name(m)) | Rhs::Take(Place::Name(m)) => {
                self.assign(st, n, &Val::Name(*m))
            }
            Rhs::Read(Place::Field(b, f)) | Rhs::Take(Place::Field(b, f))
                if f == "length" || f == "byteLength" =>
            {
                if let Some(t) = self.length(b) {
                    st.define(Term::Val(n), &Lin::of(t));
                }
            }
            // A field's array or String, read or taken, has the field's length.
            Rhs::Read(p @ Place::Field(..)) | Rhs::Take(p @ Place::Field(..))
                if matches!(self.kind(n), Kind::Seq) =>
            {
                if let Some(t) = self.length(p) {
                    st.define(Term::Len(n), &Lin::of(t));
                }
            }
            Rhs::Make(Ctor::Array, parts) => {
                st.define(Term::Len(n), &Lin::k(parts.len() as i64));
            }
            Rhs::Call { args, .. } if matches!(self.kind(n), Kind::Seq) => {
                if let Some((_, lo, hi)) = self.resized(rhs).filter(|_| lands_on_result(args)) {
                    let len = Lin::of(Term::Len(n));
                    if lo == hi {
                        st.define(Term::Len(n), &lo);
                    } else {
                        len.sub(&lo)
                            .iter()
                            .chain(&hi.sub(&len))
                            .for_each(|l| st.assume(l));
                    }
                }
            }
            Rhs::Prim(op, vs, _) => match self.prim(st, n, op, vs) {
                Out::Def(r) => st.define(Term::Val(n), &r),
                Out::Cond(t, f) => {
                    st.conds.insert(n, (t, f));
                }
                Out::Facts(fs) => fs.iter().for_each(|f| st.assume(f)),
                Out::Nothing => {}
            },
            _ => {}
        }
        self.range(st, n);
        self.effects(st, rhs);
    }

    /// Defines `n`, just killed, as the value `v`.
    fn assign(&self, st: &mut State, n: Name, v: &Val) {
        match (v, self.kind(n)) {
            (Val::Lit(Lit::Str(s)), Kind::Seq) => {
                st.define(Term::Len(n), &Lin::k(s.len() as i64));
            }
            (Val::Name(m), Kind::Seq) => {
                st.define(Term::Len(n), &Lin::of(Term::Len(*m)));
            }
            (_, Kind::Int(..)) => {
                if let Some(l) = self.lin(v).and_then(|l| st.norm(&l)) {
                    if self.in_range(n, &l, st) {
                        st.define(Term::Val(n), &l);
                    }
                }
            }
            _ => {}
        }
    }

    /// Whether the state proves the exact value `l` lies in `n`'s type,
    /// `-EXACT..=EXACT` for an `Int64`.
    fn in_range(&self, n: Name, l: &Lin, st: &State) -> bool {
        let (lo, hi) = match self.kind(n) {
            Kind::Int(64, true) => (-EXACT, EXACT),
            Kind::Int(64, false) => (0, EXACT),
            Kind::Int(bits, true) => (-(1i64 << (bits - 1)), (1i64 << (bits - 1)) - 1),
            Kind::Int(bits, false) => (0, (1i64 << bits) - 1),
            _ => return false,
        };
        let (Some(a), Some(b)) = (l.plus(-lo), Lin::k(hi).sub(l)) else {
            return false;
        };
        st.ge0(&a).is_some() && st.ge0(&b).is_some()
    }

    /// What `n = op(vs)` states, read in `pre`.
    fn prim(&self, pre: &State, n: Name, op: &Op, vs: &[Val]) -> Out {
        let exact = |r: Option<Lin>| {
            if self.is_int64(n) {
                Self::fits(pre, r)
            } else {
                None
            }
        };
        match (op, vs) {
            (Op::Bin(o), [a, b]) => {
                let (la, lb) = (self.lin(a), self.lin(b));
                let r = match o {
                    BinOp::Add => exact(la.clone().zip(lb.clone()).and_then(|(a, b)| a.add(&b))),
                    BinOp::Sub => exact(la.clone().zip(lb.clone()).and_then(|(a, b)| a.sub(&b))),
                    BinOp::Mul => exact(la.clone().zip(lb.clone()).and_then(|(a, b)| {
                        match (a.is_const(), b.is_const()) {
                            (true, _) => b.scale(a.c),
                            (_, true) => a.scale(b.c),
                            _ => None,
                        }
                    })),
                    _ => None,
                };
                if let Some(r) = r {
                    return Out::Def(r);
                }
                let (Some(la), Some(lb)) = (la, lb) else {
                    return self.logic(pre, *o, a, b);
                };
                // An unsigned 64-bit literal above `i64::MAX` reads as a negative.
                let wide = |v: &Val| matches!(v, Val::Name(m) if matches!(self.kind(*m), Kind::Int(64, false)));
                if wide(a) || wide(b) {
                    return Out::Nothing;
                }
                let (t, f) = match o.compare() {
                    // `x > y` is `x - y - 1 >= 0`, and `x >= y` is `x - y >= 0`.
                    Some(Cmp::Order { strict, flipped }) => {
                        let (x, y) = if flipped { (&lb, &la) } else { (&la, &lb) };
                        let s = i64::from(strict);
                        let holds = x.sub(y).and_then(|d| d.plus(-s));
                        (vec![holds], vec![y.sub(x).and_then(|d| d.plus(s - 1))])
                    }
                    Some(Cmp::Equal { negated }) => {
                        let both = vec![la.sub(&lb), lb.sub(&la)];
                        if negated {
                            (vec![], both)
                        } else {
                            (both, vec![])
                        }
                    }
                    None => return self.bound(pre, n, *o, &la, &lb),
                };
                let norm = |ls: Vec<Option<Lin>>| {
                    ls.into_iter()
                        .flatten()
                        .filter_map(|l| pre.norm(&l))
                        .collect()
                };
                Out::Cond(norm(t), norm(f))
            }
            (Op::Un(UnOp::Neg), [a]) => match exact(self.lin(a).and_then(|l| l.scale(-1))) {
                Some(r) => Out::Def(r),
                None => Out::Nothing,
            },
            (Op::Un(UnOp::Not), [Val::Name(c)]) => match pre.conds.get(c) {
                Some((t, f)) => Out::Cond(f.clone(), t.clone()),
                None => Out::Nothing,
            },
            (Op::Conv(_), [a]) => match self.lin(a).and_then(|l| pre.norm(&l)) {
                Some(l) if self.in_range(n, &l, pre) => Out::Def(l),
                _ => Out::Nothing,
            },
            _ => Out::Nothing,
        }
    }

    /// The range of `n = a op b` where `b` is a literal that bounds it.
    fn bound(&self, pre: &State, n: Name, o: BinOp, la: &Lin, lb: &Lin) -> Out {
        let nonneg = pre.ge0(la).is_some();
        let (lo, hi) = match (o, lb.is_const().then_some(lb.c)) {
            (BinOp::BitAnd, Some(c)) if c >= 0 => (Lin::k(0), Lin::k(c)),
            (BinOp::Rem, Some(k)) if k >= 1 && nonneg => (Lin::k(0), Lin::k(k - 1)),
            (BinOp::Div | BinOp::Shr, Some(k)) if k >= 1 && nonneg => (Lin::k(0), la.clone()),
            _ => return Out::Nothing,
        };
        let v = Lin::of(Term::Val(n));
        Out::Facts([v.sub(&lo), hi.sub(&v)].into_iter().flatten().collect())
    }

    /// `a && b` or `a || b`: the side that gives both operands' facts.
    fn logic(&self, pre: &State, o: BinOp, a: &Val, b: &Val) -> Out {
        let (Val::Name(a), Val::Name(b)) = (a, b) else {
            return Out::Nothing;
        };
        let (Some((at, af)), Some((bt, bf))) = (pre.conds.get(a), pre.conds.get(b)) else {
            return Out::Nothing;
        };
        let both = |x: &Vec<Lin>, y: &Vec<Lin>| x.iter().chain(y).cloned().collect::<Vec<_>>();
        match o {
            BinOp::And => Out::Cond(both(at, bt), Vec::new()),
            BinOp::Or => Out::Cond(Vec::new(), both(af, bf)),
            _ => Out::Nothing,
        }
    }

    /// The receiver of a builtin call in `rhs` whose row states its length
    /// effect, and the bounds of its new length as sums over the old.
    fn resized(&self, rhs: &Rhs) -> Option<(Term, Lin, Lin)> {
        let Rhs::Call {
            callee,
            args,
            kind: Callee::Builtin,
            ..
        } = rhs
        else {
            return None;
        };
        let len = |i: usize| {
            let t = match args.get(i)? {
                (Arg::Val(Val::Name(n)), _) => self.length(&Place::Name(*n))?,
                (Arg::Place(p), _) => self.length(p)?,
                _ => return None,
            };
            Some((t, Lin::of(t)))
        };
        let (r, old) = len(0)?;
        let (lo, hi) = match prelude::builtin(callee)?.length {
            Length::Unknown => return None,
            Length::Keeps => (old.clone(), old),
            Length::GrowsByOne => (old.plus(1)?, old.plus(1)?),
            Length::ShrinksByOneIfNotEmpty => (old.plus(-1)?, old),
            Length::SetToZero => (Lin::k(0), Lin::k(0)),
            Length::GrowsByLenOf(i) => {
                let sum = old.add(&len(i)?.1)?;
                (sum.clone(), sum)
            }
            Length::SetToLenOf(i) => {
                let l = len(i)?.1;
                (l.clone(), l)
            }
        };
        Some((r, lo, hi))
    }

    /// Applies what a call in `rhs` does to the names it may write. A
    /// `modify` receiver moves its length as its row states; any other
    /// `modify` or `consume` argument rooted at a name that is not a scalar
    /// forgets it.
    fn effects(&mut self, st: &mut State, rhs: &Rhs) {
        let Rhs::Call { args, .. } = rhs else {
            return;
        };
        for (a, cap) in args {
            if let (Arg::Place(p), Capability::Modify) = (a, cap) {
                self.open(st, p);
            }
        }
        let moved = self.resized(rhs).filter(|_| !lands_on_result(args));
        for (i, (a, cap)) in args.iter().enumerate() {
            let Some(n) = root(a) else { continue };
            match (cap, &moved) {
                (Capability::Read, _) => {}
                // A scalar argument is a copy.
                (Capability::Consume, _) if matches!(self.kind(n), Kind::Int(..)) => {}
                (Capability::Modify, Some((t, lo, hi))) if i == 0 => {
                    let old = Lin::of(*t);
                    match (lo.sub(&old), hi.sub(&old)) {
                        (Some(a), Some(b)) if a.is_const() && b.is_const() => {
                            st.shift(*t, a.c, b.c)
                        }
                        _ => self.resize(st, n),
                    }
                }
                _ => self.resize(st, n),
            }
        }
    }

    /// The halving lemma. A loop that opens with `if u > c else break`
    /// (`c >= 1`), halves `u` on every turn (`u = u >> 1` or `u / 2` among
    /// [`every_turn`]'s rows), writes `u` nowhere else and has no `continue`
    /// turns at most `N = floor(log2 U)` times, `U` its type's largest value:
    /// turn `j` needs `u0 >> j >= 2`. A counter `k` whose only write is one
    /// such row `k = k + s` then lies between `k0` and `k0 + s*N` at the head,
    /// `k0` its entry value over names the loop does not write, when both fit
    /// `k`'s type. Returns those head facts.
    fn halving(&self, entry: &State, body: &[St], written: &BTreeSet<Name>) -> Vec<Lin> {
        let [St::Let(t, Rhs::Prim(Op::Bin(op), vs, _)), St::If {
            cond: Val::Name(c),
            then,
            els,
            ..
        }, rest @ ..] = body
        else {
            return Vec::new();
        };
        let (u, c1) = match (op, vs.as_slice()) {
            (BinOp::Gt, [Val::Name(u), Val::Lit(Lit::Int(k))]) => (*u, *k),
            (BinOp::GtEq, [Val::Name(u), Val::Lit(Lit::Int(k))]) => (*u, k.saturating_sub(1)),
            _ => return Vec::new(),
        };
        let Kind::Int(bits, signed) = self.kind(u) else {
            return Vec::new();
        };
        if c != t
            || c1 < 1
            || !then.is_empty()
            || !matches!(els.as_slice(), [St::Break { .. }])
            || any_continue(body)
            || writes_of(body, u) != 1
        {
            return Vec::new();
        }
        let rows = every_turn(rest);
        // `n = x op lit` among the rows, stored back into `x` by a row.
        let step = |x: Name| {
            rows.iter().find_map(|r| match r {
                St::Store {
                    place: Place::Name(p),
                    value: Val::Name(v),
                    ..
                } if *p == x => rows.iter().find_map(|d| match d {
                    St::Let(n, Rhs::Prim(Op::Bin(o), vs, _)) if n == v => match vs.as_slice() {
                        [Val::Name(y), Val::Lit(Lit::Int(k))] if *y == x => Some((*o, *k)),
                        _ => None,
                    },
                    _ => None,
                }),
                _ => None,
            })
        };
        if !matches!(step(u), Some((BinOp::Shr, 1) | (BinOp::Div, 2))) {
            return Vec::new();
        }
        // `u <= 2^(bits - 1) - 1` signed, `2^bits - 1` unsigned.
        let n = i64::from(bits) - if signed { 2 } else { 1 };
        let mut out = Vec::new();
        for r in &rows {
            let St::Store {
                place: Place::Name(k),
                ..
            } = r
            else {
                continue;
            };
            let s = match step(*k) {
                Some((BinOp::Add, s)) => s,
                Some((BinOp::Sub, s)) => s.saturating_neg(),
                _ => continue,
            };
            let k = *k;
            let Some(k0) = entry.norm(&Lin::of(Term::Val(k))) else {
                continue;
            };
            let Some(end) = s.checked_mul(n).and_then(|d| k0.plus(d)) else {
                continue;
            };
            if k == u
                || s == 0
                || writes_of(body, k) != 1
                || !matches!(self.kind(k), Kind::Int(..))
                || k0.terms.iter().any(|(t, _)| written.contains(&t.name()))
                || !self.in_range(k, &k0, entry)
                || !self.in_range(k, &end, entry)
            {
                continue;
            }
            let v = Lin::of(Term::Val(k));
            let (lo, hi) = if s > 0 { (k0, end) } else { (end, k0) };
            out.extend(v.sub(&lo));
            out.extend(hi.sub(&v));
        }
        out
    }

    fn looped(&mut self, entry: State, body: &mut [St]) -> State {
        let key = (body.as_ptr() as usize, entry);
        if !self.record {
            if let Some(exit) = self.memo.get(&key) {
                return exit.clone();
            }
        }
        let exit = self.settle(&key.1, body);
        if !self.record {
            self.memo.insert(key, exit.clone());
        }
        exit
    }

    fn settle(&mut self, entry: &State, body: &mut [St]) -> State {
        let mut written = BTreeSet::new();
        writes(body, &mut written);
        let mut indexed = Vec::new();
        seqs(body, &mut indexed);
        let mut head = entry.clone();
        for n in &written {
            head.kill(*n);
        }
        for l in self.halving(entry, body, &written) {
            head.assume(&l);
        }
        // Candidates: what entry knows about the written names, and the
        // counter shapes an index needs.
        let mut cands: BTreeSet<Lin> = BTreeSet::new();
        for f in &entry.facts {
            if f.terms.iter().any(|(t, _)| written.contains(&t.name())) {
                cands.insert(f.clone());
            }
        }
        for (t, v) in &entry.defs {
            let mentions = written.contains(&t.name())
                || v.terms.iter().any(|(x, _)| written.contains(&x.name()));
            if mentions {
                cands.extend(Lin::of(*t).sub(v));
                cands.extend(v.sub(&Lin::of(*t)));
            }
        }
        // A name a `let` in the loop binds is fresh each turn; a counter is a
        // name the loop stores to.
        let mut stored = BTreeSet::new();
        stores(body, &mut stored);
        for m in stored
            .iter()
            .filter(|m| matches!(self.kind(**m), Kind::Int(..)))
        {
            let v = Lin::of(Term::Val(*m));
            cands.insert(v.clone());
            for (b, _) in indexed.iter().filter(|(_, i)| i == m) {
                let Some(len) = self.length(b).map(Lin::of) else {
                    continue;
                };
                cands.extend(len.sub(&v));
                cands.extend(len.sub(&v).and_then(|l| l.plus(-1)));
            }
        }
        let at_entry = entry.prover();
        cands.retain(|c| at_entry.ge0(c).is_some());
        self.loops.push(Loop {
            breaks: Vec::new(),
            conts: Vec::new(),
            seen: Seen {
                written,
                stored,
                resized: BTreeSet::new(),
            },
        });
        let record = std::mem::replace(&mut self.record, false);
        loop {
            let mut h = head.clone();
            cands.iter().for_each(|c| h.assume(c));
            let end = self.block(h, body);
            let l = self.loops.last_mut().expect("pushed above");
            l.breaks.clear();
            let conts = std::mem::take(&mut l.conts);
            let ends: Vec<State> = conts.into_iter().chain([end]).collect();
            let before = cands.len();
            let provers: Vec<_> = ends.iter().map(|s| s.prover()).collect();
            cands.retain(|c| provers.iter().all(|p| p.ge0(c).is_some()));
            if cands.len() == before {
                break;
            }
        }
        self.record = record;
        cands.iter().for_each(|c| head.assume(c));
        self.block(head, body);
        let l = self.loops.pop().expect("pushed above");
        if let Some(outer) = self.loops.last_mut() {
            outer.seen.resized.extend(l.seen.resized);
        }
        State::join(&l.breaks)
    }

    /// A call took `n` by `modify` or `consume` and may have changed its length.
    fn resize(&mut self, st: &mut State, n: Name) {
        st.kill(n);
        if let Some(l) = self.loops.last_mut() {
            l.seen.resized.insert(n);
        }
    }

    fn origin_of_val(&self, v: &Val) -> Origin {
        match v {
            Val::Name(m) => self.slot[m.index()].origin,
            Val::Lit(_) => Origin::Local,
        }
    }

    /// Where the result `n` of `rhs` comes from. An operator and a builtin
    /// that reads nothing outside take the strongest origin of their
    /// operands; the length of a name is the name's.
    fn origin_of(&self, n: Name, rhs: &Rhs) -> Origin {
        let join = |vs: &mut dyn Iterator<Item = Origin>| vs.fold(Origin::Local, Origin::join);
        match rhs {
            Rhs::Val(v) => self.origin_of_val(v),
            Rhs::Read(Place::Name(m)) | Rhs::Take(Place::Name(m)) => self.slot[m.index()].origin,
            Rhs::Read(Place::Field(b, f)) | Rhs::Take(Place::Field(b, f))
                if f == "length" || f == "byteLength" =>
            {
                place_root(b).map_or(Origin::Place(n), |m| self.slot[m.index()].origin)
            }
            Rhs::Read(_) | Rhs::Take(_) => Origin::Place(n),
            Rhs::Prim(_, vs, _) => join(&mut vs.iter().map(|v| self.origin_of_val(v))),
            Rhs::Call {
                callee, args, kind, ..
            } => match kind {
                Callee::Fn(id) => Origin::Call(n, Some(*id)),
                Callee::Method | Callee::Projection | Callee::Bound | Callee::Value(_) => {
                    Origin::Call(n, None)
                }
                Callee::Builtin
                    if matches!(
                        prelude::builtin(callee).and_then(|b| b.effect),
                        Some(
                            Effect::ReadInput
                                | Effect::FsRead
                                | Effect::FsList
                                | Effect::Args
                                | Effect::Clock
                                | Effect::Random
                        )
                    ) =>
                {
                    Origin::Input(n)
                }
                _ => join(
                    &mut args
                        .iter()
                        .filter_map(|(a, _)| root(a))
                        .map(|m| self.slot[m.index()].origin),
                ),
            },
            _ => Origin::Local,
        }
    }

    /// Why the kept check `c` stays, without another call to the prover. The
    /// names are those of the first goal that fails, or the guard's operands
    /// when it states no goal. The strongest origin among them decides, then
    /// a length a call resized, then a loop that writes one of them.
    fn why(&self, st: &State, c: &Check, failing: Option<&Lin>) -> Why {
        let operands = match (failing, &c.guard) {
            (Some(_), _) => [None, None],
            (None, Guard::Range(..)) => {
                return Why::Unproved("the range is checked in std/runtime bytesOf".into())
            }
            (None, Guard::Index(p, i) | Guard::Span(p, i, _)) => {
                if self.length(p).is_none() {
                    return Why::Move(3, place_root(p));
                }
                [place_root(p), val_name(i)]
            }
            (None, Guard::NonZero(k) | Guard::Shift(k, _)) => [val_name(k), None],
            (None, Guard::NoOverflow(a, d, _)) => [val_name(a), val_name(d)],
            (None, Guard::Rule(r)) => [Some(*r), None],
        };
        let terms = || failing.into_iter().flat_map(|g| g.terms.iter());
        let names = || {
            terms()
                .map(|(t, _)| t.name())
                .chain(operands.into_iter().flatten())
        };
        let origin = names().fold(Origin::Local, |o, n| o.join(self.slot[n.index()].origin));
        match origin {
            Origin::Input(n) => return Why::Input(n),
            Origin::Call(n, f) => return Why::Callee(n, f),
            Origin::Place(n) => return Why::Move(3, Some(n)),
            _ => {}
        }
        let resized = |n: Name| self.loops.iter().any(|l| l.seen.resized.contains(&n));
        if let Some((t, _)) = terms().find(|(t, _)| !matches!(t, Term::Val(_)) && resized(t.name()))
        {
            return Why::Move(2, Some(t.name()));
        }
        if let Some(seen) = self.loops.last().map(|l| &l.seen) {
            // Exactly one name of the goal is written, and it is a counter.
            let (mut moving, mut one) = (None, true);
            for n in names().filter(|n| seen.written.contains(n)) {
                match moving {
                    None => moving = Some(n),
                    Some(m) => one &= m == n,
                }
            }
            if let Some(n) = moving.filter(|n| one && seen.stored.contains(n)) {
                return Why::Move(5, Some(n));
            }
        }
        match (origin, failing) {
            (Origin::Param(n), Some(g)) => Why::Caller(n, self.needs(st, g)),
            (_, Some(g)) => Why::Unproved(self.needs(st, g)),
            (_, None) if matches!(c.guard, Guard::Rule(_)) => {
                Why::Unproved("the prover states only equal-length `where` rules".into())
            }
            (_, None) => Why::Unproved("a guard with no linear form".into()),
        }
    }

    /// The goal `g >= 0`, over the names the state leaves free, as a sentence.
    fn needs(&self, st: &State, g: &Lin) -> std::sync::Arc<str> {
        let l = &st.norm(g).unwrap_or_else(|| g.clone());
        let mut out = String::with_capacity(64);
        out.push_str("needs ");
        for (i, (t, k)) in l.terms.iter().enumerate() {
            let n = t.name();
            let name = self.body.spoken(n);
            let temp = self.body.names[n.index()].source.starts_with('@');
            out.push_str(match (i, *k < 0) {
                (0, false) => "",
                (0, true) => "-",
                (_, false) => " + ",
                (_, true) => " - ",
            });
            if k.abs() != 1 {
                let _ = write!(out, "{} * ", k.abs());
            }
            let _ = match t {
                Term::Val(_) => write!(out, "{name}"),
                _ if temp => write!(out, "len({name})"),
                _ => write!(out, "{name}.length"),
            };
        }
        let _ = match (l.terms.is_empty(), l.c) {
            (true, c) => write!(out, "{c}"),
            (false, 0) => Ok(()),
            (false, c) => write!(out, " {} {}", if c < 0 { '-' } else { '+' }, c.abs()),
        };
        out.push_str(" >= 0");
        out.into()
    }
}

fn val_name(v: &Val) -> Option<Name> {
    match v {
        Val::Name(n) => Some(*n),
        Val::Lit(_) => None,
    }
}

/// The rows a loop runs on every turn that reaches its end: the body's own
/// rows and those of the blocks among them.
fn every_turn(ss: &[St]) -> Vec<&St> {
    ss.iter()
        .flat_map(|s| match s {
            St::Block { body, .. } => every_turn(body),
            s => vec![s],
        })
        .collect()
}

/// How many rows of `ss` write `n`: a `let`, a store, a binder, or a call
/// argument other than `read`.
fn writes_of(ss: &[St], n: Name) -> usize {
    let by_call = |rhs: &Rhs| match rhs {
        Rhs::Call { args, .. } => args
            .iter()
            .filter(|(a, c)| *c != Capability::Read && root(a) == Some(n))
            .count(),
        _ => 0,
    };
    ss.iter()
        .map(|s| match s {
            St::Let(m, rhs) => usize::from(*m == n) + by_call(rhs),
            St::Do { rhs, .. } => by_call(rhs),
            St::Store { place, .. } => usize::from(place_root(place) == Some(n)),
            St::If { then, els, .. } => writes_of(then, n) + writes_of(els, n),
            St::Loop { body, .. } | St::Block { body, .. } => writes_of(body, n),
            St::Switch { arms, .. } => arms
                .iter()
                .map(|a| usize::from(a.binds.contains(&n)) + writes_of(&a.body, n))
                .sum(),
            _ => 0,
        })
        .sum()
}

fn any_continue(ss: &[St]) -> bool {
    ss.iter().any(|s| match s {
        St::Continue { .. } => true,
        St::If { then, els, .. } => any_continue(then) || any_continue(els),
        St::Loop { body, .. } | St::Block { body, .. } => any_continue(body),
        St::Switch { arms, .. } => arms.iter().any(|a| any_continue(&a.body)),
        _ => false,
    })
}

/// The leaves of a predicate's top-level `&&` tree.
fn conjuncts(e: &vyrn_frontend::ast::Expr) -> usize {
    match e {
        vyrn_frontend::ast::Expr::Binary {
            op: BinOp::And,
            lhs,
            rhs,
            ..
        } => conjuncts(lhs) + conjuncts(rhs),
        _ => 1,
    }
}

/// Whether a builtin's length effect lands on its result: its receiver is not
/// passed `modify`, so the call hands the resized array back (`@push`).
fn lands_on_result(args: &[(Arg, Capability)]) -> bool {
    args.first().is_some_and(|(_, c)| *c != Capability::Modify)
}

/// Whether a proved verdict is one the emitter can honour: `bytesOf` checks
/// a range inside and has no unchecked form.
fn provable(g: &Guard) -> bool {
    !matches!(g, Guard::Range(..))
}

/// The name whose lengths a store into `p` may change: the name itself, or
/// the record whose field it replaces. A store into an element keeps every
/// length.
fn resized_by_store(p: &Place) -> Option<Name> {
    match p {
        Place::Name(n) => Some(*n),
        Place::Field(r, _) => match &**r {
            Place::Name(r) => Some(*r),
            _ => None,
        },
        _ => None,
    }
}

/// The name an argument writes through.
fn root(a: &Arg) -> Option<Name> {
    match a {
        Arg::Val(Val::Name(n)) => Some(*n),
        Arg::Place(p) => place_root(p),
        Arg::Val(Val::Lit(_)) => None,
    }
}

fn place_root(p: &Place) -> Option<Name> {
    match p {
        Place::Name(n) => Some(*n),
        Place::Field(b, _) | Place::Elem(b, _) | Place::Key(b, _) => place_root(b),
        Place::Global(_) => None,
    }
}

/// Every name the rows bind, store, or hand a call to write.
fn writes(ss: &[St], out: &mut BTreeSet<Name>) {
    for s in ss {
        match s {
            St::Let(_, rhs) | St::Do { rhs, .. } => {
                if let St::Let(n, _) = s {
                    out.insert(*n);
                }
                if let Rhs::Call { args, .. } = rhs {
                    for (a, cap) in args {
                        if *cap != Capability::Read {
                            out.extend(root(a));
                        }
                    }
                }
            }
            St::Store { place, .. } => out.extend(resized_by_store(place)),
            St::If { then, els, .. } => {
                writes(then, out);
                writes(els, out);
            }
            St::Loop { body, .. } | St::Block { body, .. } => writes(body, out),
            St::Switch { arms, .. } => {
                for a in arms {
                    out.extend(a.binds.iter().copied());
                    writes(&a.body, out);
                }
            }
            _ => {}
        }
    }
}

/// A slot per name of `body`, marked relevant when a check's goal can depend
/// on the name: a name a check compares, and every name a definition or a comparison links to a
/// relevant one, either way.
fn relevant(body: &Body, ss: &[St]) -> Vec<Slot> {
    let blank = Slot {
        relevant: false,
        origin: Origin::Local,
    };
    let mut rel = vec![blank; body.names.len()];
    loop {
        let before = rel.iter().filter(|r| r.relevant).count();
        mark(body, ss, &mut rel);
        if rel.iter().filter(|r| r.relevant).count() == before {
            return rel;
        }
    }
}

fn mark(body: &Body, ss: &[St], rel: &mut [Slot]) {
    let val = |v: &Val| match v {
        Val::Name(n) => Some(*n),
        Val::Lit(_) => None,
    };
    for s in ss {
        match s {
            St::Check(c) => {
                let (p, vs): (Option<Name>, Vec<&Val>) = match &c.guard {
                    Guard::Index(p, i) | Guard::Span(p, i, _) => (place_root(p), vec![i]),
                    Guard::Shift(k, _) | Guard::NonZero(k) => (None, vec![k]),
                    Guard::NoOverflow(_, d, _) => (None, vec![d]),
                    Guard::Range(..) => (None, vec![]),
                    Guard::Rule(r) => (Some(*r), vec![]),
                };
                for n in p.into_iter().chain(vs.into_iter().filter_map(val)) {
                    rel[n.index()].relevant = true;
                }
            }
            St::Let(n, rhs) => {
                let from: Vec<Name> = match rhs {
                    Rhs::Val(v) => val(v).into_iter().collect(),
                    Rhs::Read(p) | Rhs::Take(p) => match p {
                        Place::Name(m) => vec![*m],
                        Place::Field(b, f) if f == "length" || f == "byteLength" => {
                            place_root(b).into_iter().collect()
                        }
                        Place::Field(b, _) if matches!(**b, Place::Name(_)) => {
                            place_root(p).into_iter().collect()
                        }
                        _ => vec![],
                    },
                    Rhs::Prim(_, vs, _) => vs.iter().filter_map(val).collect(),
                    Rhs::Call {
                        callee,
                        args,
                        kind: Callee::Builtin,
                        ..
                    } if prelude::builtin(callee).is_some_and(|b| b.length != Length::Unknown) => {
                        args.iter().filter_map(|(a, _)| root(a)).collect()
                    }
                    _ => vec![],
                };
                if rel[n.index()].relevant || from.iter().any(|m| rel[m.index()].relevant) {
                    rel[n.index()].relevant = true;
                    from.iter().for_each(|m| rel[m.index()].relevant = true);
                }
            }
            St::Store { place, value, .. } => {
                if let (Some(n), Some(m)) = (resized_by_store(place), val(value)) {
                    if rel[n.index()].relevant || rel[m.index()].relevant {
                        rel[n.index()].relevant = true;
                        rel[m.index()].relevant = true;
                    }
                }
            }
            St::If { then, els, .. } => {
                mark(body, then, rel);
                mark(body, els, rel);
            }
            St::Loop { body: l, .. } | St::Block { body: l, .. } => mark(body, l, rel),
            St::Switch { arms, .. } => arms.iter().for_each(|a| mark(body, &a.body, rel)),
            _ => {}
        }
    }
}

/// Every name a row stores to.
fn stores(ss: &[St], out: &mut BTreeSet<Name>) {
    for s in ss {
        match s {
            St::Store {
                place: Place::Name(n),
                ..
            } => {
                out.insert(*n);
            }
            St::If { then, els, .. } => {
                stores(then, out);
                stores(els, out);
            }
            St::Loop { body, .. } | St::Block { body, .. } => stores(body, out),
            St::Switch { arms, .. } => arms.iter().for_each(|a| stores(&a.body, out)),
            _ => {}
        }
    }
}

/// Every place a check row indexes by a name, with that name.
fn seqs(ss: &[St], out: &mut Vec<(Place, Name)>) {
    for s in ss {
        match s {
            St::Check(c) => {
                if let Guard::Index(b, Val::Name(i)) | Guard::Span(b, Val::Name(i), _) = &c.guard {
                    out.push((b.clone(), *i));
                }
            }
            St::If { then, els, .. } => {
                seqs(then, out);
                seqs(els, out);
            }
            St::Loop { body, .. } | St::Block { body, .. } => seqs(body, out),
            St::Switch { arms, .. } => arms.iter().for_each(|a| seqs(&a.body, out)),
            _ => {}
        }
    }
}
