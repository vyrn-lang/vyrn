//! Linear facts over the names of one core body: the domain the check-elision
//! pass ([`crate::elide`]) proves checks in.
//!
//! A [`Lin`] is an exact integer sum `c + k1*t1 + k2*t2 ..` over [`Term`]s. A
//! fact is a `Lin` known to be `>= 0`. A [`State`] holds facts and definitions
//! `term = Lin`; a defined term appears in no fact and no other definition, so
//! [`State::norm`] substitutes once. Every operation on a `Lin` is checked: an
//! overflow answers `None` and proves nothing.
//!
//! [`State::ge0`] answers with a [`Cert`]: the premises it used, each a fact of
//! the state or a length axiom, whose sum the goal exceeds by a constant.
//! [`Cert::verify`] checks that sum again in `i128`, sharing no code with the
//! search.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use vyrn_frontend::core::Name;

/// A value the facts speak about: an integer name, the length of an array or
/// String name (bytes for a String), or the length of a record name's array or
/// String field. A `Col` names its field by the least index among the fields
/// the record's `where` rule states of equal length, so one term is the
/// length of each of them; inside a group of stores into the record's
/// fields, by the field's own index (`elide::Walk::open`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Term {
    Val(Name),
    Len(Name),
    Col(Name, u32),
}

impl Term {
    pub fn name(self) -> Name {
        match self {
            Term::Val(n) | Term::Len(n) | Term::Col(n, _) => n,
        }
    }
}

/// `c + sum(k * t)`, terms sorted, no zero coefficient.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Lin {
    pub terms: Vec<(Term, i64)>,
    pub c: i64,
}

impl Lin {
    pub fn k(c: i64) -> Lin {
        Lin {
            terms: Vec::new(),
            c,
        }
    }

    pub fn of(t: Term) -> Lin {
        Lin {
            terms: vec![(t, 1)],
            c: 0,
        }
    }

    pub fn is_const(&self) -> bool {
        self.terms.is_empty()
    }

    fn coef(&self, t: Term) -> i64 {
        self.terms
            .iter()
            .find(|(x, _)| *x == t)
            .map_or(0, |(_, k)| *k)
    }

    fn mentions_name(&self, n: Name) -> bool {
        self.terms.iter().any(|(t, _)| t.name() == n)
    }

    fn mentions(&self, t: Term) -> bool {
        self.coef(t) != 0
    }

    pub fn add(&self, o: &Lin) -> Option<Lin> {
        let (a, b) = (&self.terms, &o.terms);
        let mut terms = Vec::with_capacity(a.len() + b.len());
        let (mut i, mut j) = (0, 0);
        while i < a.len() && j < b.len() {
            let (x, y) = (a[i], b[j]);
            let (t, k) = match x.0.cmp(&y.0) {
                Ordering::Less => {
                    i += 1;
                    x
                }
                Ordering::Greater => {
                    j += 1;
                    y
                }
                Ordering::Equal => {
                    i += 1;
                    j += 1;
                    (x.0, x.1.checked_add(y.1)?)
                }
            };
            if k != 0 {
                terms.push((t, k));
            }
        }
        terms.extend_from_slice(&a[i..]);
        terms.extend_from_slice(&b[j..]);
        Some(Lin {
            terms,
            c: self.c.checked_add(o.c)?,
        })
    }

    pub fn scale(&self, m: i64) -> Option<Lin> {
        if m == 0 {
            return Some(Lin::k(0));
        }
        Some(Lin {
            terms: self
                .terms
                .iter()
                .map(|(t, k)| Some((*t, k.checked_mul(m)?)))
                .collect::<Option<_>>()?,
            c: self.c.checked_mul(m)?,
        })
    }

    pub fn sub(&self, o: &Lin) -> Option<Lin> {
        self.add(&o.scale(-1)?)
    }

    pub fn plus(&self, c: i64) -> Option<Lin> {
        self.add(&Lin::k(c))
    }

    /// `self` with each term `t` replaced by `by(t)`; `None` when a term has
    /// no replacement or the sum overflows.
    pub fn map(&self, by: impl Fn(Term) -> Option<Lin>) -> Option<Lin> {
        let mut out = Lin::k(self.c);
        for (t, k) in &self.terms {
            out = out.add(&by(*t)?.scale(*k)?)?;
        }
        Some(out)
    }

    /// `self` with `t` replaced by `by`.
    fn subst(&self, t: Term, by: &Lin) -> Option<Lin> {
        let k = self.coef(t);
        if k == 0 {
            return Some(self.clone());
        }
        let rest = Lin {
            terms: self
                .terms
                .iter()
                .copied()
                .filter(|(x, _)| *x != t)
                .collect(),
            c: self.c,
        };
        rest.add(&by.scale(k)?)
    }
}

/// Why a goal holds: `goal = sum(uses) + slack`, `slack >= 0`, each use a fact
/// of the state or a length axiom. A dead state proves every goal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cert {
    Dead,
    Sum { uses: Vec<Lin>, slack: i64 },
}

impl Cert {
    /// Whether the certificate proves `goal` in `st`: each use is a
    /// premise of `st`, and `goal - sum(uses)` is a constant `>= 0`, computed in
    /// `i128`.
    pub fn verify(&self, st: &State, goal: &Lin) -> bool {
        match self {
            Cert::Dead => st.dead,
            Cert::Sum { uses, .. } => {
                let Some(goal) = st.norm(goal) else {
                    return false;
                };
                if !uses.iter().all(|u| st.premise(u)) {
                    return false;
                }
                let mut sum: BTreeMap<Term, i128> = BTreeMap::new();
                let mut c = i128::from(goal.c);
                for (t, k) in &goal.terms {
                    *sum.entry(*t).or_insert(0) += i128::from(*k);
                }
                for u in uses {
                    c -= i128::from(u.c);
                    for (t, k) in &u.terms {
                        *sum.entry(*t).or_insert(0) -= i128::from(*k);
                    }
                }
                sum.values().all(|k| *k == 0) && c >= 0
            }
        }
    }
}

/// The largest length of any array or String (obligation O9).
const LEN_MAX: i64 = vyrn_frontend::trap::LENGTH_LIMIT as i64 + 1;

/// What is known at one point of a body.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct State {
    /// Each `>= 0`, normalized.
    pub facts: BTreeSet<Lin>,
    /// `term = Lin`; see the module doc for the invariant.
    pub defs: BTreeMap<Term, Lin>,
    /// Per Bool name, the facts its truth gives and those its falsehood gives.
    pub conds: BTreeMap<Name, (Vec<Lin>, Vec<Lin>)>,
    pub dead: bool,
}

impl State {
    pub fn dead() -> State {
        State {
            dead: true,
            ..State::default()
        }
    }

    pub fn norm(&self, l: &Lin) -> Option<Lin> {
        let mut out = Lin::k(l.c);
        for (t, k) in &l.terms {
            let v = match self.defs.get(t) {
                Some(d) => d.scale(*k)?,
                None => Lin {
                    terms: vec![(*t, *k)],
                    c: 0,
                },
            };
            out = out.add(&v)?;
        }
        Some(out)
    }

    /// Adds `l >= 0`. A fact that does not normalize is dropped: it would
    /// prove nothing.
    pub fn assume(&mut self, l: &Lin) {
        match self.norm(l) {
            Some(n) if n.is_const() => {
                if n.c < 0 {
                    *self = State::dead();
                }
            }
            Some(n) => {
                self.facts.insert(n);
            }
            None => {}
        }
    }

    /// Adds `a = b` as two facts.
    pub fn assume_eq(&mut self, a: &Lin, b: &Lin) {
        if let (Some(x), Some(y)) = (a.sub(b), b.sub(a)) {
            self.assume(&x);
            self.assume(&y);
        }
    }

    /// Defines the fresh term `t` as `v`. `t` must hold no definition and
    /// appear in no fact: the caller has [`State::kill`]ed it.
    pub fn define(&mut self, t: Term, v: &Lin) {
        if let Some(n) = self.norm(v).filter(|n| n.coef(t) == 0) {
            self.defs.insert(t, n);
        }
    }

    /// Whether `u` is a premise of this state: a fact, or a length axiom.
    fn premise(&self, u: &Lin) -> bool {
        self.facts.contains(u) || axioms(u.terms.iter().map(|(t, _)| *t)).contains(u)
    }

    /// Forgets everything about `n` and its lengths. A definition `d = s*x +
    /// rest` with `s` one or minus one first restates every fact about `x`
    /// through `d`, so nothing known is lost to an exact rename.
    pub fn kill(&mut self, n: Name) {
        let cols: BTreeSet<Term> = (self.defs.iter())
            .flat_map(|(d, v)| std::iter::once(*d).chain(v.terms.iter().map(|(t, _)| *t)))
            .filter(|t| matches!(t, Term::Col(m, _) if *m == n))
            .collect();
        for t in [Term::Val(n), Term::Len(n)].into_iter().chain(cols) {
            self.defs.remove(&t);
            self.restate(t);
        }
        self.conds.remove(&n);
        self.facts.retain(|f| !f.mentions_name(n));
        self.defs.retain(|_, v| !v.mentions_name(n));
        self.conds
            .retain(|_, (a, b)| !a.iter().chain(b.iter()).any(|l| l.mentions_name(n)));
    }

    /// Forgets `n` as [`State::kill`] does, keeping what the facts state
    /// without it. A definition through `n` with coefficient one or minus one
    /// restates them; otherwise each pair of facts with opposite signs on `n`
    /// sums to a fact without it (Fourier-Motzkin).
    pub fn eliminate(&mut self, n: Name) {
        let mut sums = BTreeSet::new();
        for t in [Term::Val(n), Term::Len(n)] {
            let alias = self.defs.values().any(|v| matches!(v.coef(t), 1 | -1));
            if alias || self.defs.contains_key(&t) {
                continue;
            }
            let through: Vec<Lin> = (self.facts.iter().filter(|f| f.mentions(t)).cloned())
                .chain(
                    (self.defs.iter())
                        .filter(|(_, v)| v.mentions(t))
                        .flat_map(|(d, v)| [Lin::of(*d).sub(v), v.sub(&Lin::of(*d))])
                        .flatten(),
                )
                .collect();
            for p in through.iter().filter(|p| p.coef(t) > 0) {
                for q in through.iter().filter(|q| q.coef(t) < 0) {
                    let (a, b) = (p.coef(t), q.coef(t).checked_neg());
                    let sum = b.and_then(|b| p.scale(b)?.add(&q.scale(a)?));
                    if let Some(s) = sum {
                        sums.insert(s);
                    }
                }
            }
        }
        self.kill(n);
        for s in sums.iter().filter(|s| !s.mentions_name(n)) {
            self.assume(s);
        }
    }

    /// Forgets everything about the one term `t`, restating what a definition
    /// through it knows as [`State::kill`] does.
    pub fn forget(&mut self, t: Term) {
        self.defs.remove(&t);
        self.restate(t);
        self.facts.retain(|f| !f.mentions(t));
        self.defs.retain(|_, v| !v.mentions(t));
        self.conds
            .retain(|_, (a, b)| !a.iter().chain(b.iter()).any(|l| l.mentions(t)));
    }

    /// Moves the length `t` by an unknown amount in `lo..=hi`. A fact with
    /// `k * t` holds of the new length once `-min(k*lo, k*hi)` is added; a
    /// definition of or through the length becomes its two facts first.
    pub fn shift(&mut self, t: Term, lo: i64, hi: i64) {
        let (through, defs) = std::mem::take(&mut self.defs)
            .into_iter()
            .partition(|(d, v)| *d == t || v.coef(t) != 0);
        self.defs = defs;
        let mut facts = std::mem::take(&mut self.facts);
        for (d, v) in through {
            facts.extend(Lin::of(d).sub(&v));
            facts.extend(v.sub(&Lin::of(d)));
        }
        self.conds
            .retain(|_, (a, b)| !a.iter().chain(b.iter()).any(|l| l.coef(t) != 0));
        for f in facts {
            let k = f.coef(t);
            let moved = k
                .checked_mul(lo)
                .zip(k.checked_mul(hi))
                .and_then(|(a, b)| f.plus(a.min(b).checked_neg()?));
            if let Some(f) = moved {
                self.assume(&f);
            }
        }
    }

    fn restate(&mut self, x: Term) {
        let Some((d, by)) = self.defs.iter().find_map(|(d, v)| {
            let s = v.coef(x);
            if s != 1 && s != -1 {
                return None;
            }
            // x = s*d - s*(v - s*x)
            let rest = v.sub(&Lin::of(x).scale(s)?)?;
            Some((*d, Lin::of(*d).sub(&rest)?.scale(s)?))
        }) else {
            return;
        };
        self.defs.remove(&d);
        let sub = |l: &Lin| l.subst(x, &by);
        // A length's axioms outlive the length: they bound the renamed value.
        self.facts = std::mem::take(&mut self.facts)
            .into_iter()
            .chain(axioms([x].into_iter()))
            .filter_map(|f| sub(&f))
            .collect();
        for v in self.defs.values_mut() {
            if let Some(n) = sub(v) {
                *v = n;
            }
        }
        for (a, b) in self.conds.values_mut() {
            for l in a.iter_mut().chain(b.iter_mut()) {
                if let Some(n) = sub(l) {
                    *l = n;
                }
            }
        }
    }

    /// Answers whether `goal >= 0` holds with at most two premises.
    pub fn ge0(&self, goal: &Lin) -> Option<Cert> {
        self.prover().ge0(goal)
    }

    /// The state's premises indexed once, for many queries.
    pub fn prover(&self) -> Prover<'_> {
        let mut terms: BTreeSet<Term> = BTreeSet::new();
        for f in &self.facts {
            terms.extend(f.terms.iter().map(|(t, _)| *t));
        }
        let mut best: HashMap<&[(Term, i64)], &Lin> = HashMap::new();
        for p in &self.facts {
            let e = best.entry(p.terms.as_slice()).or_insert(p);
            if p.c < e.c {
                *e = p;
            }
        }
        Prover {
            st: self,
            axioms: axioms(terms.into_iter()),
            best,
        }
    }

    /// The facts every live state of `sts` proves, the definitions they all
    /// share, and per Bool name the facts each side gives on every path
    /// ([`State::when`]).
    pub fn join(sts: &[State]) -> State {
        let live: Vec<&State> = sts.iter().filter(|s| !s.dead).collect();
        if live.len() < 2 {
            return live.first().map_or_else(State::dead, |s| (*s).clone());
        }
        let mut out = State::common(&live);
        // A name some branch does not condition loses its conditions: that
        // branch would prove a side's facts from its state alone.
        let names: Vec<Name> = (live[0].conds.keys())
            .filter(|n| live.iter().all(|s| s.conds.contains_key(n)))
            .copied()
            .collect();
        if names.is_empty() {
            return out;
        }
        let known = out.prover();
        let conds: Vec<_> = (names.into_iter())
            .map(|n| {
                let t = State::when(&live, n, &known, |c| &c.0);
                let f = State::when(&live, n, &known, |c| &c.1);
                (n, (t, f))
            })
            .collect();
        out.conds.extend(
            conds
                .into_iter()
                .filter(|(_, (t, f))| !t.is_empty() || !f.is_empty()),
        );
        out
    }

    /// The facts and definitions of [`State::join`], without conditions.
    fn common(live: &[&State]) -> State {
        let Some(first) = live.first() else {
            return State::dead();
        };
        let mut out = State::default();
        for (t, v) in &first.defs {
            if live.iter().all(|s| s.defs.get(t) == Some(v)) {
                out.defs.insert(*t, v.clone());
            }
        }
        let mut cands: BTreeSet<Lin> = BTreeSet::new();
        for s in live {
            cands.extend(s.facts.iter().cloned());
            cands.extend(s.unshared(&out));
        }
        let provers: Vec<Prover> = live.iter().map(|s| s.prover()).collect();
        for c in cands {
            if provers.iter().all(|p| p.ge0(&c).is_some()) {
                out.assume(&c);
            }
        }
        out
    }

    /// Each definition of `self` that `out` lacks, as its two facts.
    fn unshared<'a>(&'a self, out: &'a State) -> impl Iterator<Item = Lin> + 'a {
        (self.defs.iter())
            .filter(|(t, v)| out.defs.get(t) != Some(v))
            .flat_map(|(t, v)| [Lin::of(*t).sub(v), v.sub(&Lin::of(*t))])
            .flatten()
    }

    /// One side of `n`'s conditions after a join of `live` into the state
    /// `known` indexes: what every live state proves with that side assumed,
    /// less what `known` proves alone. A state the side makes dead proves
    /// every fact.
    fn when(
        live: &[&State],
        n: Name,
        known: &Prover,
        side: impl Fn(&(Vec<Lin>, Vec<Lin>)) -> &Vec<Lin>,
    ) -> Vec<Lin> {
        let given: Vec<&Vec<Lin>> = live.iter().map(|s| side(&s.conds[&n])).collect();
        if given.iter().all(|g| *g == given[0]) {
            return given[0].clone();
        }
        let mut under: Vec<State> = (live.iter().zip(&given))
            .map(|(s, g)| {
                let mut s = State {
                    facts: s.facts.clone(),
                    defs: s.defs.clone(),
                    ..State::default()
                };
                g.iter().for_each(|l| s.assume(l));
                s
            })
            .filter(|s| !s.dead)
            .collect();
        let j = match under.len() {
            1 => under.swap_remove(0),
            _ => State::common(&under.iter().collect::<Vec<_>>()),
        };
        if j.dead {
            return vec![Lin::k(-1)];
        }
        (j.facts.iter().cloned())
            .chain(j.unshared(known.st))
            .filter(|c| known.ge0(c).is_none())
            .collect()
    }
}

/// A state's facts and length axioms, with the strongest premise per term map
/// (the least constant).
pub struct Prover<'a> {
    st: &'a State,
    /// The length axioms of the facts' terms. The premises are `st.facts`, then these.
    axioms: Vec<Lin>,
    /// For each term list, the fact with the least constant.
    best: HashMap<&'a [(Term, i64)], &'a Lin>,
}

impl Prover<'_> {
    /// [`State::ge0`], over the index.
    pub fn ge0(&self, goal: &Lin) -> Option<Cert> {
        if self.st.dead {
            return Some(Cert::Dead);
        }
        let g = self.st.norm(goal)?;
        if g.is_const() {
            return (g.c >= 0).then(|| Cert::Sum {
                uses: Vec::new(),
                slack: g.c,
            });
        }
        // The goal's own lengths may appear in no fact.
        let extra = axioms(g.terms.iter().map(|(t, _)| *t));
        let one = |d: &Lin| {
            let fact = self.best.get(d.terms.as_slice()).copied();
            let axiom = self.axioms.iter().filter(|p| p.terms == d.terms);
            // The premise with the least constant, the first on a tie.
            let indexed = fact
                .into_iter()
                .chain(axiom)
                .reduce(|m, p| if p.c < m.c { p } else { m });
            indexed
                .into_iter()
                .chain(extra.iter().filter(|p| p.terms == d.terms))
                .find(|p| p.c <= d.c)
        };
        if let Some(p) = one(&g) {
            return Some(Cert::Sum {
                slack: g.c - p.c,
                uses: vec![p.clone()],
            });
        }
        for p in self.st.facts.iter().chain(&self.axioms).chain(&extra) {
            let Some(d) = g.sub(p) else { continue };
            if let Some(q) = one(&d) {
                return Some(Cert::Sum {
                    slack: d.c - q.c,
                    uses: vec![p.clone(), q.clone()],
                });
            }
        }
        None
    }
}

/// `len >= 0` and `LEN_MAX - len >= 0` for every length among `terms`.
fn axioms(terms: impl Iterator<Item = Term>) -> Vec<Lin> {
    let mut out = Vec::new();
    for t in terms {
        if let Term::Len(_) | Term::Col(..) = t {
            out.push(Lin::of(t));
            out.push(Lin {
                terms: vec![(t, -1)],
                c: LEN_MAX,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(n: u32) -> Lin {
        Lin::of(Term::Val(Name(n)))
    }

    #[test]
    fn a_sum_that_overflows_is_no_form() {
        assert_eq!(Lin::k(i64::MAX).plus(1), None);
        assert_eq!(v(0).scale(i64::MIN).and_then(|l| l.scale(-1)), None);
    }

    #[test]
    fn a_goal_follows_from_two_facts_and_the_certificate_checks() {
        let mut st = State::default();
        // i >= 0, len(b) - i - 1 >= 0
        st.assume(&v(0));
        let len = Lin::of(Term::Len(Name(1)));
        st.assume(&len.sub(&v(0)).unwrap().plus(-1).unwrap());
        let goal = Lin::k(1 << 40).sub(&v(0)).unwrap();
        let cert = st.ge0(&goal).expect("i < len <= LEN_MAX");
        assert!(cert.verify(&st, &goal));
        assert!(st.ge0(&v(0).plus(-1).unwrap()).is_none());
    }

    #[test]
    fn a_certificate_with_a_premise_the_state_lacks_fails() {
        let st = State::default();
        let cert = Cert::Sum {
            uses: vec![v(0)],
            slack: 0,
        };
        assert!(!cert.verify(&st, &v(0)));
    }

    #[test]
    fn a_kill_restates_through_an_exact_definition() {
        let mut st = State::default();
        st.assume(&v(0));
        st.define(Term::Val(Name(1)), &v(0).plus(1).unwrap());
        st.kill(Name(0));
        assert!(st.ge0(&v(1).plus(-1).unwrap()).is_some());
    }

    #[test]
    fn a_kill_forgets_a_defined_field_length() {
        let mut st = State::default();
        let col = Term::Col(Name(0), 1);
        st.define(col, &Lin::k(3));
        st.kill(Name(0));
        assert!(st.ge0(&Lin::of(col).plus(-3).unwrap()).is_none());
    }

    #[test]
    fn a_kill_of_a_length_keeps_its_axioms_on_the_rename() {
        let mut st = State::default();
        st.define(Term::Val(Name(0)), &Lin::of(Term::Len(Name(1))));
        st.kill(Name(1));
        assert!(st.ge0(&v(0)).is_some());
    }

    #[test]
    fn a_shrink_by_at_most_one_keeps_the_old_length_as_a_bound() {
        let mut st = State::default();
        let len = Lin::of(Term::Len(Name(1)));
        st.define(Term::Val(Name(0)), &len);
        st.shift(Term::Len(Name(1)), -1, 0);
        assert!(st.ge0(&v(0).sub(&len).unwrap()).is_some());
        assert!(st.ge0(&len.sub(&v(0)).unwrap().plus(1).unwrap()).is_some());
        assert!(st.ge0(&len.sub(&v(0)).unwrap()).is_none());
    }

    #[test]
    fn an_elimination_keeps_the_sum_of_opposite_bounds() {
        let mut st = State::default();
        // j - w >= 0 and w >= 0 give j >= 0 once w is gone.
        st.assume(&v(0).sub(&v(1)).unwrap());
        st.assume(&v(1));
        st.eliminate(Name(1));
        assert!(st.facts.iter().all(|f| !f.mentions_name(Name(1))));
        assert!(st.ge0(&v(0)).is_some());
    }

    #[test]
    fn a_join_keeps_only_what_every_branch_proves() {
        let (mut a, mut b) = (State::default(), State::default());
        a.assume(&v(0).plus(-2).unwrap());
        b.assume(&v(0).plus(-1).unwrap());
        let j = State::join(&[a, b]);
        assert!(j.ge0(&v(0).plus(-1).unwrap()).is_some());
        assert!(j.ge0(&v(0).plus(-2).unwrap()).is_none());
    }

    /// A callee's facts join the caller's state as premises, so the order in
    /// which summaries settle must not decide a proof: a fact added to a
    /// state never loses a goal it proved.
    #[test]
    fn more_facts_never_lose_a_proof() {
        let len = |n: u32| Lin::of(Term::Len(Name(n)));
        let facts = [
            v(0),
            v(1).plus(-1).unwrap(),
            len(2).sub(&v(0)).unwrap().plus(-1).unwrap(),
            v(0).sub(&v(1)).unwrap(),
            len(2).sub(&v(1)).unwrap(),
            Lin::k(5).sub(&v(1)).unwrap(),
        ];
        let goals = [
            v(0),
            v(1),
            len(2).sub(&v(0)).unwrap(),
            len(2).sub(&v(1)).unwrap().plus(-1).unwrap(),
            Lin::k(LEN_MAX).sub(&v(0)).unwrap(),
            Lin::k(4).sub(&v(1)).unwrap(),
            v(0).sub(&v(1)).unwrap().plus(1).unwrap(),
        ];
        // Every subset of the facts, against every superset of it.
        for small in 0u32..1 << facts.len() {
            let mut st = State::default();
            (0..facts.len())
                .filter(|i| small & 1 << i != 0)
                .for_each(|i| st.assume(&facts[i]));
            for big in (0u32..1 << facts.len()).filter(|b| b & small == small) {
                let mut more = st.clone();
                (0..facts.len())
                    .filter(|i| big & 1 << i != 0)
                    .for_each(|i| more.assume(&facts[i]));
                for g in &goals {
                    if st.ge0(g).is_some() {
                        let cert = more
                            .ge0(g)
                            .expect("a superset of the facts proves the goal");
                        assert!(cert.verify(&more, g));
                    }
                }
            }
        }
    }
}
