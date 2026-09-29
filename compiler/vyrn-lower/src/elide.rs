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

use vyrn_frontend::ast::{BinOp, Capability, Type, TypeDecl, UnOp};
use vyrn_frontend::prelude::{self, Length};
use vyrn_frontend::prim::Cmp;

use crate::check::{Guard, Verdict};
use crate::core::{Arg, Body, Callee, Ctor, Lit, Name, Op, Place, Rhs, St, Val};
use crate::facts::{Lin, State, Term};

/// The bound an exact `Int64` sum must provably stay within: `2^62`, so no
/// premise or goal of the prover itself leaves `i64`.
const EXACT: i64 = 1 << 62;

/// Marks every check row of `body`, and of each lambda body it holds, that
/// cannot fail [`Verdict::Proved`].
pub fn decide(body: &mut Body, decls: &HashMap<String, TypeDecl>) {
    for l in &mut body.lambdas {
        decide(l, decls);
    }
    if !any_check(&body.stmts) {
        return;
    }
    let mut stmts = std::mem::take(&mut body.stmts);
    let mut w = Walk {
        body,
        decls,
        loops: Vec::new(),
        record: true,
        memo: HashMap::new(),
        relevant: relevant(body, &stmts),
    };
    let mut st = State::default();
    for p in &body.params {
        w.fresh(&mut st, *p);
    }
    w.block(st, &mut stmts);
    body.stmts = stmts;
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
    loops: Vec<(Vec<State>, Vec<State>)>,
    /// Whether a check row's verdict is written: false in Houdini's rounds,
    /// true in the replay from the settled head.
    record: bool,
    /// A loop's exit state by its rows' address and its entry state, for the
    /// walks that write no verdict.
    memo: HashMap<(usize, State), State>,
    /// Per name, whether a check's goal can depend on it ([`relevant`]); the
    /// walk states nothing about any other name.
    relevant: Vec<bool>,
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
        match vyrn_frontend::types::resolve(&self.body.names[n as usize].ty, self.decls) {
            Type::Array(_) | Type::ArrayN(..) | Type::SmallArray(..) | Type::Str => Kind::Seq,
            t => match vyrn_frontend::validate::width(&t) {
                Some((bits, signed)) => Kind::Int(bits, signed),
                None => Kind::Other,
            },
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
        if !self.relevant[n as usize]
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
                let ty = vyrn_frontend::types::resolve(&self.body.names[n as usize].ty, self.decls);
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
                st
            }
            St::Store { place, value, .. } => {
                if let Place::Name(n) = place {
                    let n = *n;
                    st.kill(n);
                    if self.relevant[n as usize] {
                        self.assign(&mut st, n, value);
                        self.range(&mut st, n);
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
            St::Switch { arms, .. } => {
                let mut outs = vec![st.clone()];
                for a in arms {
                    let mut s = st.clone();
                    for b in &a.binds {
                        self.fresh(&mut s, *b);
                    }
                    outs.push(self.block(s, &mut a.body));
                }
                State::join(&outs)
            }
            St::Break { .. } => {
                if let Some((b, _)) = self.loops.last_mut() {
                    b.push(st);
                }
                State::dead()
            }
            St::Continue { .. } => {
                if let Some((_, c)) = self.loops.last_mut() {
                    c.push(st);
                }
                State::dead()
            }
            St::Return { .. } | St::Trap => State::dead(),
            St::Check(c) => {
                let goals = self.goals(&c.guard);
                if self.record {
                    let proved = goals.as_ref().is_some_and(|gs| {
                        gs.iter()
                            .all(|g| st.ge0(g).is_some_and(|cert| cert.verify(&st, g)))
                    });
                    if proved && provable(&c.guard) {
                        c.verdict = Verdict::Proved;
                    }
                }
                // A passed index, span or shift check states its goals; a
                // divisor's goals are only one way it can pass.
                if matches!(
                    c.guard,
                    Guard::Index(..) | Guard::Span(..) | Guard::Shift(..)
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
        let len = |p: &Place| match p {
            Place::Name(b) if matches!(self.kind(*b), Kind::Seq) => Some(Lin::of(Term::Len(*b))),
            _ => None,
        };
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
        })
    }

    fn bind(&mut self, st: &mut State, n: Name, rhs: &Rhs) {
        st.kill(n);
        if !self.relevant[n as usize] {
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
                if let Place::Name(b) = &**b {
                    if matches!(self.kind(*b), Kind::Seq) {
                        st.define(Term::Val(n), &Lin::of(Term::Len(*b)));
                    }
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
    fn resized(&self, rhs: &Rhs) -> Option<(Name, Lin, Lin)> {
        let Rhs::Call {
            callee,
            args,
            kind: Callee::Builtin,
            ..
        } = rhs
        else {
            return None;
        };
        let len = |i: usize| match args.get(i)? {
            (Arg::Val(Val::Name(n)) | Arg::Place(Place::Name(n)), _)
                if matches!(self.kind(*n), Kind::Seq) =>
            {
                Some((*n, Lin::of(Term::Len(*n))))
            }
            _ => None,
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
    fn effects(&self, st: &mut State, rhs: &Rhs) {
        let Rhs::Call { args, .. } = rhs else {
            return;
        };
        let moved = self.resized(rhs).filter(|_| !lands_on_result(args));
        for (a, cap) in args {
            let Some(n) = root(a) else { continue };
            match (cap, &moved) {
                (Capability::Read, _) => {}
                // A scalar argument is a copy.
                (Capability::Consume, _) if matches!(self.kind(n), Kind::Int(..)) => {}
                (Capability::Modify, Some((r, lo, hi))) if *r == n => {
                    let old = Lin::of(Term::Len(n));
                    match (lo.sub(&old), hi.sub(&old)) {
                        (Some(a), Some(b)) if a.is_const() && b.is_const() => st.shift(n, a.c, b.c),
                        _ => st.kill(n),
                    }
                }
                _ => st.kill(n),
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
                || k0.terms.iter().any(|(t, _)| written.contains(&name_of(*t)))
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
        let mut indexed = BTreeSet::new();
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
            if f.terms.iter().any(|(t, _)| written.contains(&name_of(*t))) {
                cands.insert(f.clone());
            }
        }
        for (t, v) in &entry.defs {
            let mentions = written.contains(&name_of(*t))
                || v.terms.iter().any(|(x, _)| written.contains(&name_of(*x)));
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
                let len = Lin::of(Term::Len(*b));
                cands.extend(len.sub(&v));
                cands.extend(len.sub(&v).and_then(|l| l.plus(-1)));
            }
        }
        let at_entry = entry.prover();
        cands.retain(|c| at_entry.ge0(c).is_some());
        let record = std::mem::replace(&mut self.record, false);
        loop {
            let mut h = head.clone();
            cands.iter().for_each(|c| h.assume(c));
            self.loops.push((Vec::new(), Vec::new()));
            let end = self.block(h, body);
            let (_, conts) = self.loops.pop().expect("pushed above");
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
        self.loops.push((Vec::new(), Vec::new()));
        self.block(head, body);
        let (breaks, _) = self.loops.pop().expect("pushed above");
        State::join(&breaks)
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

fn name_of(t: Term) -> Name {
    match t {
        Term::Val(n) | Term::Len(n) => n,
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
            St::Store {
                place: Place::Name(n),
                ..
            } => {
                out.insert(*n);
            }
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

/// Per name of `body`, whether a check's goal can depend on it: a name a
/// check compares, and every name a definition or a comparison links to a
/// relevant one, either way.
fn relevant(body: &Body, ss: &[St]) -> Vec<bool> {
    let mut rel = vec![false; body.names.len()];
    loop {
        let before = rel.iter().filter(|r| **r).count();
        mark(ss, &mut rel);
        if rel.iter().filter(|r| **r).count() == before {
            return rel;
        }
    }
}

fn mark(ss: &[St], rel: &mut [bool]) {
    let val = |v: &Val| match v {
        Val::Name(n) => Some(*n),
        Val::Lit(_) => None,
    };
    for s in ss {
        match s {
            St::Check(c) => {
                let (p, vs): (Option<&Place>, Vec<&Val>) = match &c.guard {
                    Guard::Index(p, i) | Guard::Span(p, i, _) => (Some(p), vec![i]),
                    Guard::Shift(k, _) | Guard::NonZero(k) => (None, vec![k]),
                    Guard::NoOverflow(_, d, _) => (None, vec![d]),
                    Guard::Range(..) => (None, vec![]),
                };
                for n in p
                    .and_then(place_root)
                    .into_iter()
                    .chain(vs.into_iter().filter_map(val))
                {
                    rel[n as usize] = true;
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
                if rel[*n as usize] || from.iter().any(|m| rel[*m as usize]) {
                    rel[*n as usize] = true;
                    from.iter().for_each(|m| rel[*m as usize] = true);
                }
            }
            St::Store {
                place: Place::Name(n),
                value,
                ..
            } => {
                if let Some(m) = val(value) {
                    if rel[*n as usize] || rel[m as usize] {
                        rel[*n as usize] = true;
                        rel[m as usize] = true;
                    }
                }
            }
            St::If { then, els, .. } => {
                mark(then, rel);
                mark(els, rel);
            }
            St::Loop { body, .. } | St::Block { body, .. } => mark(body, rel),
            St::Switch { arms, .. } => arms.iter().for_each(|a| mark(&a.body, rel)),
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

/// Every array or String name a check row indexes by a name, with that name.
fn seqs(ss: &[St], out: &mut BTreeSet<(Name, Name)>) {
    for s in ss {
        match s {
            St::Check(c) => {
                if let Guard::Index(Place::Name(b), Val::Name(i))
                | Guard::Span(Place::Name(b), Val::Name(i), _) = &c.guard
                {
                    out.insert((*b, *i));
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
