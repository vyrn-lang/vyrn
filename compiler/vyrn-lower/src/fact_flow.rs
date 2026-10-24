//! The fact judgment: which arrays are sorted, and by which order, over the
//! core of every body, after the ownership and effect judgments.
//!
//! `core::augment` builds one [`Facts`] and calls [`Facts::judge`] on each
//! body it built, after the effect judgment. The judgment writes only
//! refusals. No pass after it reads its state, and the check prover never
//! takes a fact as a premise, so a wrong fact can wrongly accept a program
//! but never moves emitted code.
//!
//! A fact lives on a name: `sorted<O>` or `strict<O>` of an array. The walk
//! gains and loses facts through the rows. A parameter holds what its clause
//! requires; an array of at most one element holds every fact; a move or a
//! store carries the facts of its value; a call's result holds what its
//! callee's result clause guarantees. A push keeps a fact only where an
//! order atom shows the value not before the array's last (`sorted`) or
//! after it (`strict`); any other write drops the facts of the array it
//! writes. Order atoms relate terms, a value or an array's last element, and
//! come from `before`'s answers and from pops of a sorted array, whose new
//! last is not after the value popped. An atom about the last of an array
//! the walk knows empty holds. A loop's head keeps what holds at entry and
//! after every turn (Houdini).
//!
//! The walk refuses only where a fact is needed: at a call whose parameter
//! clause requires it, and at a return the result clause guarantees it of.
//! The sentence names the row that dropped the fact, where one did.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use vyrn_frontend::ast::{Capability, FnId, Function, Program, Type};
use vyrn_frontend::core::check::{guarantees, requirements, Guarantee, Requirement, Word};
use vyrn_frontend::core::{Arg, Body, Callee, Ctor, Lit, Name, Place, Rhs, St, Test, Val};
use vyrn_frontend::diagnostics::Diagnostic;
use vyrn_frontend::movecheck::Refusal;
use vyrn_frontend::order::{Known, Order};
use vyrn_frontend::rule;
use vyrn_frontend::rules::Rule;
use vyrn_frontend::types::Decls;

use crate::FnRow;

/// The fact clauses of one program's functions and its `before`: what the
/// judgment reads at each call and return. The placer builds one before its
/// loop over the built bodies and calls [`Facts::judge`] on each, after the
/// effect judgment ran.
pub(crate) struct Facts<'a> {
    functions: &'a [Function],
    /// Per function with a fact clause, by its index in `functions`.
    clauses: HashMap<usize, (Vec<Requirement>, Vec<Guarantee>)>,
    before: Option<usize>,
    rows: Rows<'a>,
}

impl<'a> Facts<'a> {
    /// The facts of `program`, whose function table is `rows`. `None`, with
    /// nothing allocated, for a program that links no `std/order`, or that
    /// states no fact and declares no `before`.
    pub(crate) fn of(program: &'a Program, rows: &'a [FnRow], decls: &dyn Decls) -> Option<Self> {
        let orders = &program.orders;
        if orders.is_empty() {
            return None;
        }
        let mut clauses = HashMap::new();
        for (i, f) in program.functions.iter().enumerate() {
            if f.params.iter().all(|p| p.clause.is_none()) && f.ret_clause.is_none() {
                continue;
            }
            // A clause the checker refused states nothing.
            let r = requirements(f, decls, orders).unwrap_or_default();
            let g = guarantees(f, decls, orders).unwrap_or_default();
            if !r.is_empty() || !g.is_empty() {
                clauses.insert(i, (r, g));
            }
        }
        let before = orders.name(Known::Before);
        let before = (program.functions.iter()).position(|f| Some(f.name.as_str()) == before);
        if clauses.is_empty() && before.is_none() {
            return None;
        }
        let rows = Rows {
            rows,
            declared: program.functions.len(),
        };
        Some(Facts {
            functions: &program.functions,
            clauses,
            before,
            rows,
        })
    }

    /// Appends the refusals of `top` and of its lambdas to `out`. `top` is
    /// the instance of `program.functions[decl]` at `subst`, or a body no
    /// function declares where `decl` is `None`. Allocates nothing for a body
    /// that calls no function with a fact clause and no `before`, and whose
    /// own result clause states no fact.
    pub(crate) fn judge(
        &self,
        decl: Option<FnId>,
        subst: &BTreeMap<String, Type>,
        top: &Body,
        out: &mut Vec<Refusal>,
    ) {
        let (assumed, own) = match decl.and_then(|d| self.clauses.get(&d.index())) {
            Some((r, g)) => (&r[..], &g[..]),
            None => (&[][..], &[][..]),
        };
        if own.is_empty() && !self.calls(top) {
            return;
        }
        let subst: HashMap<String, Type> = subst.clone().into_iter().collect();
        let first = out.len();
        for (k, body) in top.frames().into_iter().enumerate() {
            let mut j = Judge {
                body,
                facts: self,
                name: decl.map_or(&top.name, |d| &self.functions[d.index()].name),
                own: if k == 0 { own } else { &[] },
                subst: &subst,
                orders: Vec::new(),
                record: false,
                loops: Vec::new(),
                refused: Vec::new(),
            };
            // A lambda's parameters are no declaration's.
            let entry = j.entry(if k == 0 { assumed } else { &[] });
            j.list(entry.clone(), &body.stmts);
            j.record = true;
            j.list(entry, &body.stmts);
            for (line, r) in j.refused {
                let d = Diagnostic::refusal(line, 0, "check", r).in_file(body.file.clone());
                let seen = |o: &Refusal| {
                    let o = &o.diagnostic;
                    o.file == d.file && o.line == d.line && o.message == d.message
                };
                if !out[first..].iter().any(seen) {
                    out.push(Refusal {
                        diagnostic: d,
                        body: top.name.clone(),
                    });
                }
            }
        }
    }

    /// Whether a frame of `top` calls a function with a fact clause or
    /// `before`.
    fn calls(&self, top: &Body) -> bool {
        let fact = |s: &St| match s {
            St::Let(
                _,
                Rhs::Call {
                    kind: Callee::Fn(g),
                    ..
                },
            )
            | St::Do {
                rhs:
                    Rhs::Call {
                        kind: Callee::Fn(g),
                        ..
                    },
                ..
            } => (self.rows.decl(g.index()))
                .is_some_and(|(d, _)| self.clauses.contains_key(&d) || Some(d) == self.before),
            _ => false,
        };
        any(&top.stmts, &fact) || top.lambdas.iter().any(|l| self.calls(l))
    }
}

/// Whether a row of `ss`, or a row under one, satisfies `p`.
fn any(ss: &[St], p: &impl Fn(&St) -> bool) -> bool {
    ss.iter().any(|s| p(s) || s.lists().any(|l| any(l, p)))
}

/// A term of an order atom: a value's name, the last element of an array
/// name, or the element a pop's `Option` holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Term {
    Val(Name),
    Last(Name),
    Popped(Name),
}

impl Term {
    fn names(self, n: Name) -> bool {
        matches!(self, Term::Val(m) | Term::Last(m) | Term::Popped(m) if m == n)
    }
}

/// `lo <= hi`, or `lo < hi` where `strict`, under the canonical order
/// `order` ([`Judge::atom`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Atom {
    lo: Term,
    hi: Term,
    strict: bool,
    order: usize,
}

/// A fact an array holds: sorted, or strictly sorted, by `order`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Held {
    strict: bool,
    order: usize,
}

/// The kind of row that dropped a fact, for a sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Write {
    Push,
    Store,
    Call,
}

impl Write {
    fn word(self) -> &'static str {
        match self {
            Write::Push => "push",
            Write::Store => "store",
            Write::Call => "call",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
struct State {
    dead: bool,
    held: BTreeMap<Name, BTreeSet<Held>>,
    /// Arrays known empty: each atom about the last element of one holds.
    empty: BTreeSet<Name>,
    /// Arrays known to hold at most one element, a superset of `empty`: each
    /// holds every fact.
    single: BTreeSet<Name>,
    atoms: BTreeSet<Atom>,
    /// Per Bool name, the atoms its truth and its falsehood give.
    conds: BTreeMap<Name, [Vec<Atom>; 2]>,
    /// Per `Option` name a pop bound, the array it popped, the tag of
    /// `Some`, and whether the array was known empty, so `Some` is dead.
    pops: BTreeMap<Name, (Name, u64, bool)>,
    /// Per array, the row that last dropped one of its facts, and its line.
    lost: BTreeMap<Name, (Write, usize)>,
}

impl State {
    fn dead() -> State {
        State {
            dead: true,
            ..State::default()
        }
    }

    /// Forgets everything about `n`, which the row rebinds.
    fn kill(&mut self, n: Name) {
        self.held.remove(&n);
        self.empty.remove(&n);
        self.single.remove(&n);
        self.atoms.retain(|a| !a.lo.names(n) && !a.hi.names(n));
        self.conds.remove(&n);
        let about = |c: &[Atom]| c.iter().any(|a| a.lo.names(n) || a.hi.names(n));
        self.conds.retain(|_, [yes, no]| !about(yes) && !about(no));
        self.pops.retain(|t, (xs, _, _)| *t != n && *xs != n);
        self.lost.remove(&n);
    }

    /// Drops the facts of the array `n` and the atoms about its last element,
    /// recording the row that did.
    fn lose(&mut self, n: Name, write: Write, line: usize) {
        self.empty.remove(&n);
        if self.held.remove(&n).is_some_and(|h| !h.is_empty()) | self.single.remove(&n) {
            self.lost.insert(n, (write, line));
        }
        self.atoms
            .retain(|a| a.lo != Term::Last(n) && a.hi != Term::Last(n));
    }

    /// `to`, a fresh name, takes what the state knows of `from`.
    fn copy(&mut self, from: Name, to: Name) {
        if let Some(h) = self.held.get(&from).cloned() {
            self.held.insert(to, h);
        }
        if self.empty.contains(&from) {
            self.empty.insert(to);
        }
        if self.single.contains(&from) {
            self.single.insert(to);
        }
        if let Some(l) = self.lost.get(&from).copied() {
            self.lost.insert(to, l);
        }
        if let Some(c) = self.conds.get(&from).cloned() {
            self.conds.insert(to, c);
        }
        let swap = |t: Term| match t {
            Term::Val(m) if m == from => Term::Val(to),
            Term::Last(m) if m == from => Term::Last(to),
            t => t,
        };
        let more: Vec<Atom> = (self.atoms.iter())
            .filter(|a| a.lo.names(from) || a.hi.names(from))
            .map(|a| Atom {
                lo: swap(a.lo),
                hi: swap(a.hi),
                ..*a
            })
            .collect();
        self.atoms.extend(more);
    }

    /// Every atom about `from` made about `to`.
    fn rename(&mut self, from: Term, to: Term) {
        let swap = |t: Term| if t == from { to } else { t };
        self.atoms = (self.atoms.iter())
            .map(|a| Atom {
                lo: swap(a.lo),
                hi: swap(a.hi),
                ..*a
            })
            .collect();
    }

    /// Whether the atom holds: it is stated, chains from stated atoms, or is
    /// about the last element of an array the state knows empty.
    fn proves(&self, a: Atom) -> bool {
        let vacuous = |t: Term| matches!(t, Term::Last(m) if self.empty.contains(&m));
        if self.dead || vacuous(a.lo) || vacuous(a.hi) || (a.lo == a.hi && !a.strict) {
            return true;
        }
        // A chain from `lo` to `hi`, strict where one step is.
        let mut seen: BTreeSet<(Term, bool)> = BTreeSet::new();
        let mut todo = vec![(a.lo, false)];
        while let Some((t, strict)) = todo.pop() {
            if t == a.hi && (strict || !a.strict) {
                return true;
            }
            if !seen.insert((t, strict)) {
                continue;
            }
            for x in self
                .atoms
                .iter()
                .filter(|x| x.order == a.order && x.lo == t)
            {
                todo.push((x.hi, strict || x.strict));
            }
        }
        false
    }
}

/// One walk over one body.
struct Judge<'a> {
    body: &'a Body,
    facts: &'a Facts<'a>,
    /// The declaration's linked name, which a sentence names.
    name: &'a str,
    /// What the body's own result clause guarantees.
    own: &'a [Guarantee],
    /// The instance's type arguments, by parameter name.
    subst: &'a HashMap<String, Type>,
    /// The orders the walk met, by index; an atom's are canonical.
    orders: Vec<Order>,
    /// Whether the walk records refusals: the last walk does.
    record: bool,
    /// Per enclosing loop, the states its `continue`s and `break`s leave.
    loops: Vec<(Vec<State>, Vec<State>)>,
    refused: Vec<(usize, Rule)>,
}

/// Rounds a loop head may take before it keeps nothing; each round only
/// drops what a turn breaks, so the corpus's loops settle in two or three.
const ROUNDS: usize = 32;

impl Judge<'_> {
    fn id(&mut self, o: Order) -> usize {
        match self.orders.iter().position(|x| *x == o) {
            Some(i) => i,
            None => {
                self.orders.push(o);
                self.orders.len() - 1
            }
        }
    }

    /// The state at entry: each parameter holds what its clause requires,
    /// which every call proved.
    fn entry(&mut self, assumed: &[Requirement]) -> State {
        // The orders the result clause names, met before any row.
        for Guarantee(f) in self.own {
            self.order_of(&f.order, &[]);
        }
        let mut st = State::default();
        for r in assumed {
            let (Some(order), Some(p)) = (
                self.order_of(&r.fact.order, &[]),
                self.body.params.get(r.param),
            ) else {
                continue;
            };
            let h = Held {
                strict: r.fact.word == Word::Strict,
                order,
            };
            st.held.entry(*p).or_default().insert(h);
        }
        st
    }

    /// `lo <= hi` (`lo < hi` where `strict`) under `o`, in `o`'s canonical
    /// direction.
    fn atom(&mut self, lo: Term, hi: Term, strict: bool, o: usize) -> Atom {
        let (c, flipped) = self.orders[o].canonical();
        let order = self.id(c);
        let (lo, hi) = if flipped { (hi, lo) } else { (lo, hi) };
        Atom {
            lo,
            hi,
            strict,
            order,
        }
    }

    /// The order `t` names under the instance's and the call's type
    /// arguments; `None` where it names none.
    fn order_of(&mut self, t: &Type, solved: &[(String, Type)]) -> Option<usize> {
        let call: HashMap<String, Type> = solved.iter().cloned().collect();
        let t = vyrn_frontend::types::substitute(t, &call);
        match vyrn_frontend::types::substitute(&t, self.subst) {
            Type::Order(o) => Some(self.id(*o)),
            _ => None,
        }
    }

    /// Whether `n` holds `want`: an array of one element holds every fact, a strict
    /// fact gives the sorted one, a finer order gives a coarser one, and a
    /// strict fact of a coarser order gives a finer one.
    fn holds(&self, st: &State, n: Name, want: Held) -> bool {
        if st.dead || st.single.contains(&n) {
            return true;
        }
        let w = &self.orders[want.order];
        st.held.get(&n).into_iter().flatten().any(|h| {
            let o = &self.orders[h.order];
            match want.strict {
                false => o.refines(w) || (h.strict && w.refines(o)),
                true => h.strict && w.refines(o),
            }
        })
    }

    fn join(&self, a: State, b: State) -> State {
        if a.dead {
            return b;
        }
        if b.dead {
            return a;
        }
        let mut held: BTreeMap<Name, BTreeSet<Held>> = BTreeMap::new();
        for n in a.held.keys().chain(b.held.keys()) {
            let mine = |x: &State, y: &State| -> Vec<Held> {
                (x.held.get(n).into_iter().flatten())
                    .filter(|h| self.holds(y, *n, **h))
                    .copied()
                    .collect()
            };
            let keep: BTreeSet<Held> = mine(&a, &b).into_iter().chain(mine(&b, &a)).collect();
            if !keep.is_empty() {
                held.insert(*n, keep);
            }
        }
        let atoms = (a.atoms.iter().chain(&b.atoms))
            .filter(|x| a.proves(**x) && b.proves(**x))
            .copied()
            .collect();
        let mut lost = a.lost.clone();
        for (n, l) in &b.lost {
            lost.entry(*n).or_insert(*l);
        }
        State {
            dead: false,
            held,
            empty: a.empty.intersection(&b.empty).copied().collect(),
            single: a.single.intersection(&b.single).copied().collect(),
            atoms,
            conds: (a.conds.iter())
                .filter(|(n, c)| b.conds.get(n) == Some(c))
                .map(|(n, c)| (*n, c.clone()))
                .collect(),
            pops: (a.pops.iter())
                .filter(|(n, x)| b.pops.get(n) == Some(x))
                .map(|(n, x)| (*n, *x))
                .collect(),
            lost,
        }
    }

    fn list(&mut self, mut st: State, ss: &[St]) -> State {
        for s in ss {
            if st.dead {
                break;
            }
            st = self.stmt(st, s);
        }
        st
    }

    fn stmt(&mut self, mut st: State, s: &St) -> State {
        match s {
            St::Let(n, rhs) => {
                let line = self.body.names[n.index()].line;
                self.rhs(&mut st, Some(*n), rhs, line);
                st
            }
            St::Do { rhs, line, .. } => {
                self.rhs(&mut st, None, rhs, *line);
                st
            }
            St::Store {
                place, value, line, ..
            } => {
                match place {
                    Place::Name(x) => {
                        let had = st.held.get(x).is_some_and(|h| !h.is_empty());
                        st.kill(*x);
                        if let Val::Name(t) = value {
                            st.copy(*t, *x);
                        }
                        if had && !st.held.contains_key(x) {
                            st.lost.insert(*x, (Write::Store, *line));
                        }
                    }
                    p => {
                        if let Some(r) = root(p) {
                            st.lose(r, Write::Store, *line);
                        }
                    }
                }
                st
            }
            St::If {
                cond: Val::Lit(Lit::Bool(b)),
                then,
                els,
                ..
            } => self.list(st, if *b { then } else { els }),
            St::If {
                cond, then, els, ..
            } => {
                let [yes, no] = match cond {
                    Val::Name(c) => st.conds.get(c).cloned().unwrap_or_default(),
                    Val::Lit(_) => Default::default(),
                };
                let mut t = st.clone();
                t.atoms.extend(yes);
                let mut f = st;
                f.atoms.extend(no);
                let t = self.list(t, then);
                let f = self.list(f, els);
                self.join(t, f)
            }
            St::Switch { on, arms, .. } => {
                let popped = match on {
                    Val::Name(t) => {
                        (st.pops.get(t).copied()).map(|(xs, some, none)| (*t, xs, some, none))
                    }
                    Val::Lit(_) => None,
                };
                let mut out = State::dead();
                for (i, a) in arms.iter().enumerate() {
                    let mut here = st.clone();
                    // The rows that bind the binders out of the scrutinee
                    // carry nothing a fact reads.
                    let head = a.reads(on).len();
                    if let Some((t, xs, some, none)) = popped {
                        let tried = |tag: bool| {
                            (arms[..i].iter())
                                .any(|e| matches!(e.test, Test::Tag(x) if (x == some) == tag))
                        };
                        // Whether the arm runs on `Some`; `None` where the
                        // walk cannot tell.
                        let present = match a.test {
                            Test::Tag(x) => Some(x == some),
                            Test::Else if tried(true) => Some(false),
                            Test::Else if tried(false) => Some(true),
                            Test::Else | Test::Holds(_) => None,
                        };
                        match (present, a.binds.as_slice()) {
                            (Some(true), _) if none => here = State::dead(),
                            (Some(true), [b]) => here.rename(Term::Popped(t), Term::Val(*b)),
                            (Some(false), _) => {
                                here.empty.insert(xs);
                                here.single.insert(xs);
                            }
                            _ => {}
                        }
                        here.atoms
                            .retain(|x| x.lo != Term::Popped(t) && x.hi != Term::Popped(t));
                    }
                    let end = self.list(here, &a.body[head..]);
                    out = self.join(out, end);
                }
                out
            }
            St::Loop { body, .. } => self.lp(st, body),
            St::Block { body, .. } => self.list(st, body),
            St::Break { .. } => {
                if let Some((_, breaks)) = self.loops.last_mut() {
                    breaks.push(st);
                }
                State::dead()
            }
            St::Continue { .. } => {
                if let Some((conts, _)) = self.loops.last_mut() {
                    conts.push(st);
                }
                State::dead()
            }
            St::Return { value, line, .. } => {
                if let Some(Val::Name(r)) = value {
                    self.returns(&st, *r, *line);
                }
                State::dead()
            }
            St::Trap => State::dead(),
            St::Drop(..) | St::Row { .. } | St::Check(_) => st,
        }
    }

    /// The loop over `body` from `st`: the head is the greatest state that
    /// holds at entry and after every turn, and the exit joins the breaks.
    fn lp(&mut self, st: State, body: &[St]) -> State {
        let record = self.record;
        self.record = false;
        let mut head = st.clone();
        let mut settled = false;
        for _ in 0..ROUNDS {
            let back = self.turn(head.clone(), body).0;
            let next = self.join(head.clone(), back);
            if next == head {
                settled = true;
                break;
            }
            head = next;
        }
        if !settled {
            head = State {
                lost: st.lost.clone(),
                ..State::default()
            };
        }
        self.record = record;
        let (_, breaks) = self.turn(head, body);
        breaks
            .into_iter()
            .fold(State::dead(), |a, b| self.join(a, b))
    }

    /// One turn of a loop from `head`: the state at the back edge, and the
    /// states its breaks leave.
    fn turn(&mut self, head: State, body: &[St]) -> (State, Vec<State>) {
        self.loops.push((Vec::new(), Vec::new()));
        let end = self.list(head, body);
        let (conts, breaks) = self.loops.pop().expect("pushed above");
        let back = conts.into_iter().fold(end, |a, b| self.join(a, b));
        (back, breaks)
    }

    fn rhs(&mut self, st: &mut State, n: Option<Name>, rhs: &Rhs, line: usize) {
        if let Some(n) = n {
            st.kill(n);
        }
        match rhs {
            Rhs::Make(Ctor::Array, parts) if parts.is_empty() => {
                if let Some(n) = n {
                    st.empty.insert(n);
                    st.single.insert(n);
                }
            }
            Rhs::Val(Val::Name(m)) | Rhs::Read(Place::Name(m)) | Rhs::Take(Place::Name(m)) => {
                if let Some(n) = n {
                    st.copy(*m, n);
                }
            }
            Rhs::Call {
                callee,
                args,
                kind,
                solved,
                ..
            } => match (kind, callee.as_str(), args.as_slice(), n) {
                (Callee::Builtin, "@push", [(Arg::Val(Val::Name(xs)), _), (v, _)], Some(t)) => {
                    self.push(st, *xs, v, t, line)
                }
                // A copy is the same sequence.
                (Callee::Reserved, "@copy", [(Arg::Val(Val::Name(xs)), _)], Some(t)) => {
                    st.copy(*xs, t)
                }
                (Callee::Builtin, "@pop", [(xs, _)], Some(t)) => {
                    match (arg_root(xs), some_tag(rhs)) {
                        (Some(xs), Some(some)) => self.pop(st, xs, t, some),
                        _ => self.writes(st, args, line),
                    }
                }
                (Callee::Fn(g), _, _, _) => match self.facts.rows.decl(g.index()) {
                    // An instance's row holds its type arguments; a call
                    // into a generic body, which no instance judges, its own.
                    Some((d, [])) => self.call(st, d, args, solved, n, line),
                    Some((d, of)) => self.call(st, d, args, of, n, line),
                    None => self.writes(st, args, line),
                },
                _ => self.writes(st, args, line),
            },
            _ => {}
        }
    }

    /// `t = xs.push(v)`: `t` keeps each fact of `xs` the push cannot break,
    /// and its last element is `v`.
    fn push(&mut self, st: &mut State, xs: Name, v: &Arg, t: Name, line: usize) {
        let value = match v {
            Arg::Val(Val::Name(v)) => Some(Term::Val(*v)),
            _ => None,
        };
        let held = self.held(st, xs);
        let mut kept = BTreeSet::new();
        for h in held {
            let ok = match value {
                Some(v) => {
                    let a = self.atom(Term::Last(xs), v, h.strict, h.order);
                    st.proves(a)
                }
                None => false,
            };
            if ok {
                kept.insert(h);
            }
        }
        let broke = (st.held.get(&xs).into_iter().flatten()).any(|h| !kept.contains(h));
        let one = st.empty.contains(&xs);
        st.atoms
            .retain(|a| a.lo != Term::Last(xs) && a.hi != Term::Last(xs));
        if let Some(v) = value {
            st.rename(v, Term::Last(t));
        }
        let lost = match broke {
            true => Some((Write::Push, line)),
            false => st.lost.get(&xs).copied(),
        };
        if one {
            st.single.insert(t);
        }
        if !kept.is_empty() {
            st.held.insert(t, kept);
        }
        if let Some(l) = lost {
            st.lost.insert(t, l);
        }
        st.kill(xs);
    }

    /// `t = xs.pop()`: the element `t` holds is what `xs`'s last was, and
    /// each fact of `xs` puts the new last not after it.
    fn pop(&mut self, st: &mut State, xs: Name, t: Name, some: u64) {
        let held = self.held(st, xs);
        st.rename(Term::Last(xs), Term::Popped(t));
        for h in held {
            let a = self.atom(Term::Last(xs), Term::Popped(t), h.strict, h.order);
            st.atoms.insert(a);
        }
        let none = st.empty.contains(&xs);
        if st.single.contains(&xs) {
            st.empty.insert(xs);
        }
        st.pops.insert(t, (xs, some, none));
    }

    /// The facts of `xs` a push or a pop reads. An array of at most one
    /// element holds every fact, so the walk asks those of the orders it has
    /// met, both ways; a pop leaves it empty, so its atoms about the new last
    /// hold, and stating them keeps them through a join with a state where
    /// the other array they relate is empty.
    fn held(&mut self, st: &State, xs: Name) -> Vec<Held> {
        let mut held: Vec<Held> = st.held.get(&xs).into_iter().flatten().copied().collect();
        if st.single.contains(&xs) && !st.empty.contains(&xs) {
            for o in 0..self.orders.len() {
                let down = self.id(Order::desc(self.orders[o].clone()));
                for order in [o, down] {
                    held.extend([false, true].map(|strict| Held { strict, order }));
                }
            }
        }
        held
    }

    /// A call to the declared function `g`: its requirements of the
    /// arguments, its writes, and its result's guarantees.
    fn call(
        &mut self,
        st: &mut State,
        g: usize,
        args: &[(Arg, Capability)],
        solved: &[(String, Type)],
        n: Option<Name>,
        line: usize,
    ) {
        if Some(g) == self.facts.before {
            let order = solved
                .iter()
                .find(|(p, _)| p == "O")
                .map(|(_, t)| t.clone());
            let o = order.and_then(|t| self.order_of(&t, &[]));
            if let (Some(o), Some(c), [(a, _), (b, _)]) = (o, n, args) {
                if let (Some(Val::Name(a)), Some(Val::Name(b))) = (a.val(), b.val()) {
                    let yes = self.atom(Term::Val(*a), Term::Val(*b), true, o);
                    let no = self.atom(Term::Val(*b), Term::Val(*a), false, o);
                    st.conds.insert(c, [vec![yes], vec![no]]);
                }
            }
            return;
        }
        let clauses = &self.facts.clauses;
        let Some((reqs, guars)) = clauses.get(&g) else {
            return self.writes(st, args, line);
        };
        for r in reqs {
            let Some(order) = self.order_of(&r.fact.order, solved) else {
                continue;
            };
            let want = Held {
                strict: r.fact.word == Word::Strict,
                order,
            };
            let Some(a) = args.get(r.param).and_then(|(a, _)| arg_root(a)) else {
                continue;
            };
            if !self.holds(st, a, want) {
                let callee = self.body.spelled(&self.facts.functions[g].name).to_string();
                self.refuse_need(st, line, &callee, a, want);
            }
        }
        self.writes(st, args, line);
        let Some(n) = n else { return };
        for Guarantee(f) in guars {
            if let Some(order) = self.order_of(&f.order, solved) {
                let h = Held {
                    strict: f.word == Word::Strict,
                    order,
                };
                st.held.entry(n).or_default().insert(h);
            }
        }
    }

    /// What a call does to its `modify` and `consume` arguments: drops their
    /// facts, and forgets a consumed name.
    fn writes(&mut self, st: &mut State, args: &[(Arg, Capability)], line: usize) {
        for (a, cap) in args {
            let Some(r) = arg_root(a) else { continue };
            match cap {
                Capability::Read => {}
                Capability::Modify => st.lose(r, Write::Call, line),
                Capability::Consume => st.kill(r),
            }
        }
    }

    /// Checks the body's result clause at a return of `r`.
    fn returns(&mut self, st: &State, r: Name, line: usize) {
        for Guarantee(f) in self.own {
            let Some(order) = self.order_of(&f.order, &[]) else {
                continue;
            };
            let want = Held {
                strict: f.word == Word::Strict,
                order,
            };
            if !self.record || self.holds(st, r, want) {
                continue;
            }
            let name = self.body.spelled(self.name).to_string();
            let (word, order) = self.said(want);
            let rule = match st.lost.get(&r) {
                Some((w, at)) => {
                    rule!(
                        ResultFactBroken,
                        name,
                        word,
                        order,
                        write = w.word(),
                        at = *at
                    )
                }
                None => rule!(ResultFactMissing, name, word, order),
            };
            self.refused.push((line, rule));
        }
    }

    /// Refuses a call to `callee` whose argument `a` does not hold `want`.
    fn refuse_need(&mut self, st: &State, line: usize, callee: &str, a: Name, want: Held) {
        if !self.record {
            return;
        }
        let arg = self.spoken(a);
        let (word, order) = self.said(want);
        let other = (st.held.get(&a).into_iter().flatten()).find(|h| !h.strict || want.strict);
        let rule = match (st.lost.get(&a), other) {
            (Some((w, at)), _) => {
                rule!(
                    FactBroken,
                    callee,
                    arg,
                    word,
                    order,
                    write = w.word(),
                    at = *at
                )
            }
            (None, Some(h)) => {
                let held = Type::Order(Box::new(self.orders[h.order].clone()));
                rule!(FactOtherOrder, callee, arg, word, order, held)
            }
            (None, None) => rule!(FactMissing, callee, arg, word, order),
        };
        self.refused.push((line, rule));
    }

    /// The fact `h` as a sentence says it: its word and its order.
    fn said(&self, h: Held) -> (&'static str, Type) {
        let word = if h.strict {
            "strictly sorted"
        } else {
            "sorted"
        };
        (word, Type::Order(Box::new(self.orders[h.order].clone())))
    }

    /// `n` as the reader wrote it, quoted, or the line of a temporary.
    fn spoken(&self, n: Name) -> String {
        let s = self.body.spoken(n);
        match self.body.names[n.index()].source.starts_with('@') {
            true => s.into_owned(),
            false => format!("`{s}`"),
        }
    }
}

/// The World's function table, read by declaration.
struct Rows<'a> {
    rows: &'a [FnRow],
    /// The table's first rows are `program.functions`, in order
    /// ([`vyrn_frontend::ast::Program::source_names`]).
    declared: usize,
}

impl Rows<'_> {
    /// The declaration row `id` is the body of, as an index into
    /// `program.functions`, and its instance's type arguments: empty for a
    /// declaration's own row. `None` for a test, a lambda frame or the
    /// module state.
    fn decl(&self, id: usize) -> Option<(usize, &[(String, Type)])> {
        if id < self.declared {
            return Some((id, &[]));
        }
        match &self.rows.get(id)?.generic {
            Some((g, args)) if g.index() < self.declared => Some((g.index(), args)),
            _ => None,
        }
    }
}

/// The tag of `Some` in the `Option` a pop row answers: its one variant
/// with a payload.
fn some_tag(rhs: &Rhs) -> Option<u64> {
    let Rhs::Call {
        ret: Some(Type::Enum(vs)),
        ..
    } = rhs
    else {
        return None;
    };
    let mut full = (0..).zip(vs).filter(|(_, v)| !v.payload.is_empty());
    match (full.next(), full.next()) {
        (Some((i, _)), None) => Some(i),
        _ => None,
    }
}

/// The name a place is rooted in.
fn root(p: &Place) -> Option<Name> {
    match p {
        Place::Name(n) => Some(*n),
        Place::Field(b, _) | Place::Elem(b, _) | Place::Key(b, _) => root(b),
        Place::Global(_) => None,
    }
}

/// The name an argument reads or writes.
fn arg_root(a: &Arg) -> Option<Name> {
    match a {
        Arg::Val(Val::Name(n)) => Some(*n),
        Arg::Val(Val::Lit(_)) => None,
        Arg::Place(p) => root(p),
    }
}
