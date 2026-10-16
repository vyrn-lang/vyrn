//! Linear facts over the names of one core body: the domain the check-elision
//! pass ([`crate::elide`]) proves checks in.
//!
//! A [`Lin`] is an exact integer sum `c + k1*t1 + k2*t2 ..` over [`Term`]s. A
//! fact is a `Lin` known to be `>= 0`; a disequality, a `Lin` known to be
//! `!= 0`. A [`State`] holds both and definitions `term = Lin`; a defined term
//! appears in no fact, disequality or other definition, so [`State::norm`]
//! substitutes once. Every operation on a `Lin` is checked: an overflow
//! answers `None` and proves nothing.
//!
//! [`State::ge0`] answers with a [`Cert`]: the premises it used, each a fact of
//! the state or a length axiom taken a whole number of times, whose sum the
//! goal exceeds by a constant.
//! [`Cert::verify`] checks that sum again in `i128`, sharing no code with the
//! search.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Deref;
use std::rc::Rc;

use vyrn_frontend::core::Name;

/// A value the facts speak about: an integer name, the length of an array or
/// String name (bytes for a String), or the length of a record name's array or
/// String field. A `Col` names its field by the least index among the fields
/// the record's `where` rule states of equal length, so one term is the
/// length of each of them; inside a group of stores into the record's
/// fields, by the field's own index (`elide::Walk::open`). A `Field` is the
/// value of a record name's integer field, by the field's index
/// (`elide::Walk::field`). An `Inner` is the length of the element an index
/// name picks from an array name, or from a record name's array field by the
/// field's own index: `Inner(r, Some(f), i)` is `r.f[i].length`. A `Ghost`
/// is an unknown bound of the elements of the parameter it numbers, above
/// them when `true`: part of no name, so no write forgets it
/// (`elide::element_rows`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Term {
    Val(Name),
    Len(Name),
    Col(Name, u32),
    Field(Name, u32),
    Inner(Name, Option<u32>, Name),
    Ghost(u32, bool),
}

impl Term {
    /// The name the term is a part of: an `Inner`'s array root. `None` for
    /// a ghost.
    pub fn name(self) -> Option<Name> {
        match self {
            Term::Val(n)
            | Term::Len(n)
            | Term::Col(n, _)
            | Term::Field(n, _)
            | Term::Inner(n, _, _) => Some(n),
            Term::Ghost(..) => None,
        }
    }

    /// Whether the term changes where `n` does: its name, or an `Inner`'s
    /// index.
    pub fn reads(self, n: Name) -> bool {
        self.name() == Some(n) || matches!(self, Term::Inner(_, _, i) if i == n)
    }

    /// Whether the term is a length: at least zero, at most `LEN_MAX`.
    pub fn is_length(self) -> bool {
        matches!(self, Term::Len(_) | Term::Col(..) | Term::Inner(..))
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

    pub fn coef(&self, t: Term) -> i64 {
        self.terms
            .iter()
            .find(|(x, _)| *x == t)
            .map_or(0, |(_, k)| *k)
    }

    fn mentions_name(&self, n: Name) -> bool {
        self.terms.iter().any(|(t, _)| t.reads(n))
    }

    fn mentions(&self, t: Term) -> bool {
        self.coef(t) != 0
    }

    pub fn add(&self, o: &Lin) -> Option<Lin> {
        self.add_scaled(o, 1)
    }

    /// `self + m * o` without a scaled copy of `o`; `None` on overflow.
    fn add_scaled(&self, o: &Lin, m: i64) -> Option<Lin> {
        let mut out = Lin::k(0);
        self.add_scaled_into(&o.terms, o.c, m, &mut out)?;
        Some(out)
    }

    /// [`Lin::add_scaled`] of the sum `c + b`, written over `out`, reusing
    /// its buffer; `out` is garbage on `None`.
    fn add_scaled_into(&self, b: &[(Term, i64)], c: i64, m: i64, out: &mut Lin) -> Option<()> {
        let a = &self.terms;
        let terms = &mut out.terms;
        terms.clear();
        terms.reserve(a.len() + b.len());
        let (mut i, mut j) = (0, 0);
        while i < a.len() && j < b.len() {
            let (x, y) = (a[i], (b[j].0, b[j].1.checked_mul(m)?));
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
        for (t, k) in &b[j..] {
            terms.push((*t, k.checked_mul(m)?));
        }
        out.c = self.c.checked_add(c.checked_mul(m)?)?;
        Some(())
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
        self.add_scaled(o, -1)
    }

    pub fn plus(&self, c: i64) -> Option<Lin> {
        self.add(&Lin::k(c))
    }

    /// `self` with each term `t` replaced by `by(t)`; `None` when a term has
    /// no replacement or the sum overflows.
    pub fn map(&self, by: impl Fn(Term) -> Option<Lin>) -> Option<Lin> {
        let mut out = Lin::k(self.c);
        for (t, k) in &self.terms {
            out = out.add_scaled(&by(*t)?, *k)?;
        }
        Some(out)
    }

    /// `self` with `t` replaced by `by`.
    pub fn subst(&self, t: Term, by: &Lin) -> Option<Lin> {
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
        rest.add_scaled(by, k)
    }
}

/// What a condition states: `l >= 0`, or `l != 0`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Fact {
    Ge(Lin),
    Ne(Lin),
}

impl Fact {
    fn lin(&self) -> &Lin {
        match self {
            Fact::Ge(l) | Fact::Ne(l) => l,
        }
    }

    /// The same statement of `by(l)`.
    pub fn map(&self, by: impl FnOnce(&Lin) -> Option<Lin>) -> Option<Fact> {
        Some(match self {
            Fact::Ge(l) => Fact::Ge(by(l)?),
            Fact::Ne(l) => Fact::Ne(by(l)?),
        })
    }
}

/// Why a goal holds: `goal = sum(k * use) + slack`, `slack >= 0`, each use a
/// fact of the state or a length axiom and each `k >= 1`. A dead state proves
/// every goal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cert {
    Dead,
    Sum {
        uses: Vec<(i64, Lin)>,
        slack: i64,
    },
    /// `inner` proves the goal with every coefficient divided by `by` and the
    /// constant floored, which holds exactly when the goal does, because every
    /// term is an integer.
    Scaled {
        by: i64,
        inner: Box<Cert>,
    },
}

impl Cert {
    /// Whether the certificate proves `goal` in `st`: each use is a
    /// premise of `st` taken at least once, and `goal - sum(k * use)` is a
    /// constant `>= 0`, computed in `i128`.
    pub fn verify(&self, st: &State, goal: &Lin) -> bool {
        match self {
            Cert::Dead => st.dead,
            Cert::Sum { uses, .. } => {
                let Some(goal) = st.norm(goal) else {
                    return false;
                };
                if !uses.iter().all(|(k, u)| *k >= 1 && st.premise(u)) {
                    return false;
                }
                let mut sum: BTreeMap<Term, i128> = BTreeMap::new();
                let mut c = i128::from(goal.c);
                for (t, k) in &goal.terms {
                    *sum.entry(*t).or_insert(0) += i128::from(*k);
                }
                for (m, u) in uses {
                    let m = i128::from(*m);
                    c -= m * i128::from(u.c);
                    for (t, k) in &u.terms {
                        *sum.entry(*t).or_insert(0) -= m * i128::from(*k);
                    }
                }
                sum.values().all(|k| *k == 0) && c >= 0
            }
            Cert::Scaled { by, inner } => {
                let Some(goal) = st.norm(goal) else {
                    return false;
                };
                let terms: Option<Vec<(Term, i64)>> = (goal.terms.iter())
                    .map(|&(t, k)| (*by > 1 && k % by == 0).then_some((t, k / by)))
                    .collect();
                terms.is_some_and(|terms| {
                    let c = goal.c.div_euclid(*by);
                    inner.verify(st, &Lin { terms, c })
                })
            }
        }
    }
}

/// The largest length of any array or String (obligation O9).
const LEN_MAX: i64 = vyrn_frontend::trap::LENGTH_LIMIT as i64 + 1;

/// The bound an exact `Int64` sum must provably stay within: `2^62`, so no
/// premise or goal of the prover itself leaves `i64`.
pub const EXACT: i64 = 1 << 62;

/// The most values a join enumerates for one term ([`State::values`]).
const VALUES: i64 = 16;

/// A field that copies of a [`State`] share until one of them writes it: a
/// copy bumps a count, and the first write copies the field. It has no
/// `DerefMut`, so every write goes through [`Shared::get_mut`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Shared<T> {
    rc: Rc<T>,
}

impl<T: Clone> Shared<T> {
    /// The field to write, copied first if another state shares it.
    pub fn get_mut(&mut self) -> &mut T {
        Rc::make_mut(&mut self.rc)
    }
}

impl<T> From<T> for Shared<T> {
    fn from(t: T) -> Self {
        Shared { rc: Rc::new(t) }
    }
}

impl<T> Deref for Shared<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.rc
    }
}

// The writes below copy a shared field only when they change it.
impl<T: Ord + Clone> Shared<BTreeSet<T>> {
    pub fn insert(&mut self, t: T) {
        if !self.contains(&t) {
            self.get_mut().insert(t);
        }
    }

    pub fn remove(&mut self, t: &T) {
        if self.contains(t) {
            self.get_mut().remove(t);
        }
    }

    pub fn retain(&mut self, keep: impl Fn(&T) -> bool) {
        if !self.iter().all(&keep) {
            self.get_mut().retain(keep);
        }
    }
}

impl<K: Ord + Clone, V: Clone> Shared<BTreeMap<K, V>> {
    pub fn insert(&mut self, k: K, v: V) {
        self.get_mut().insert(k, v);
    }

    pub fn remove(&mut self, k: &K) {
        if self.contains_key(k) {
            self.get_mut().remove(k);
        }
    }

    pub fn retain(&mut self, keep: impl Fn(&K, &V) -> bool) {
        if !self.iter().all(|(k, v)| keep(k, v)) {
            self.get_mut().retain(|k, v| keep(k, v));
        }
    }
}

/// A Bool name's conditions: the facts its truth gives and those its
/// falsehood gives. States that copy them share them.
pub type Sides = Shared<(Vec<Fact>, Vec<Fact>)>;

/// What is known at one point of a body. A copy shares each field until it
/// writes it ([`Shared`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct State {
    /// Each `>= 0`, normalized.
    pub facts: Shared<BTreeSet<Lin>>,
    /// Each `!= 0`, normalized, in the form [`canon`] gives.
    pub ne: Shared<BTreeSet<Lin>>,
    /// `term = Lin`; see the module doc for the invariant.
    pub defs: Shared<BTreeMap<Term, Lin>>,
    /// Per Bool name, the facts its truth gives and those its falsehood gives.
    pub conds: Shared<BTreeMap<Name, Sides>>,
    /// The borrows of a global still equal to it: their lengths are the
    /// global's (`elide::Walk::borrowed`).
    pub fresh: Shared<BTreeSet<Name>>,
    /// Per array name, the templates every element meets: each a `Lin`
    /// `>= 0` with the array's own [`Term::Val`] for the element. A template
    /// is no fact: an empty array meets every template, so two of them could
    /// prove a false length. The walk instantiates one at an element read
    /// (`elide::Walk::element_read`), whose check shows the element exists.
    pub elems: Shared<BTreeMap<Name, BTreeSet<Lin>>>,
    pub dead: bool,
}

thread_local! {
    /// The state every [`State::default`] shares, so a default state allocates
    /// nothing until it is written.
    static EMPTY: State = State {
        facts: Shared::default(),
        ne: Shared::default(),
        defs: Shared::default(),
        conds: Shared::default(),
        fresh: Shared::default(),
        elems: Shared::default(),
        dead: false,
    };
}

impl Default for State {
    fn default() -> State {
        EMPTY.with(State::clone)
    }
}

impl State {
    pub fn dead() -> State {
        State {
            dead: true,
            ..State::default()
        }
    }

    /// `l` with each defined term replaced by its definition; `None` on
    /// overflow. A sum is sorted with no zero coefficient, so one that names
    /// no defined term is its own normal form and comes back borrowed.
    pub fn norm<'l>(&self, l: &'l Lin) -> Option<Cow<'l, Lin>> {
        debug_assert!(l.terms.windows(2).all(|w| w[0].0 < w[1].0));
        debug_assert!(l.terms.iter().all(|(_, k)| *k != 0));
        if !l.terms.iter().any(|(t, _)| self.defs.contains_key(t)) {
            return Some(Cow::Borrowed(l));
        }
        // Every scaled term in the order a left fold of `add` meets it, so a
        // stable sort and a fold per term overflow exactly where `add` does.
        let mut parts = Vec::with_capacity(l.terms.len());
        let mut c = l.c;
        for (t, k) in &l.terms {
            match self.defs.get(t) {
                Some(d) => {
                    for (u, j) in &d.terms {
                        parts.push((*u, j.checked_mul(*k)?));
                    }
                    c = c.checked_add(d.c.checked_mul(*k)?)?;
                }
                None => parts.push((*t, *k)),
            }
        }
        parts.sort_by_key(|(t, _)| *t);
        let mut terms: Vec<(Term, i64)> = Vec::with_capacity(parts.len());
        for (t, k) in parts {
            match terms.last_mut() {
                Some((u, j)) if *u == t => *j = j.checked_add(k)?,
                _ => terms.push((t, k)),
            }
        }
        terms.retain(|(_, k)| *k != 0);
        Some(Cow::Owned(Lin { terms, c }))
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
                if !self.facts.contains(&*n) {
                    self.facts.insert(n.into_owned());
                }
            }
            None => {}
        }
    }

    /// Adds `f`.
    pub fn add(&mut self, f: &Fact) {
        match f {
            Fact::Ge(l) => self.assume(l),
            Fact::Ne(l) => self.differ(l),
        }
    }

    /// Adds `l != 0`, then raises the bounds it touches ([`State::tighten`]).
    pub fn differ(&mut self, l: &Lin) {
        match self.norm(l).map(Cow::into_owned).and_then(canon) {
            Some(n) if n.is_const() => {
                if n.c == 0 {
                    *self = State::dead();
                }
            }
            Some(n) => {
                self.ne.insert(n);
                self.tighten();
            }
            None => {}
        }
    }

    /// Whether the state holds `l != 0` without a bound: `l` is a nonzero
    /// constant or a disequality.
    pub fn differs(&self, l: &Lin) -> bool {
        match self.norm(l).map(Cow::into_owned).and_then(canon) {
            Some(n) if n.is_const() => n.c != 0,
            Some(n) => self.ne.contains(&n),
            None => false,
        }
    }

    /// For each disequality `d != 0` and each sign `e` of `d`, raises
    /// `e >= 0` to `e - 1 >= 0`. A round adds such a bound or stops, and a
    /// pair of disequality and sign adds at most one, since `e - 1 >= 0` then
    /// holds. So two rounds per disequality bound the loop.
    fn tighten(&mut self) {
        for _ in 0..=2 * self.ne.len() {
            let p = self.prover();
            let raised: Vec<Lin> = (self.ne.iter())
                .flat_map(|d| [Some(d.clone()), d.scale(-1)])
                .flatten()
                .filter_map(|e| {
                    let up = e.plus(-1)?;
                    (p.proves(&e) && !p.proves(&up)).then_some(up)
                })
                .collect();
            if raised.is_empty() {
                return;
            }
            raised.iter().for_each(|l| self.assume(l));
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
            self.defs.insert(t, n.into_owned());
        }
    }

    /// Whether `u` is a premise of this state: a fact, or a length axiom.
    fn premise(&self, u: &Lin) -> bool {
        self.facts.contains(u) || axioms(u.terms.iter().map(|(t, _)| *t)).contains(u)
    }

    /// Whether a fact or a definition names `t`.
    pub fn names(&self, t: Term) -> bool {
        self.facts.iter().any(|f| f.mentions(t))
            || (self.defs.iter()).any(|(d, v)| *d == t || v.mentions(t))
    }

    /// The terms of `n` the state names: in a fact, a disequality, a
    /// definition or a condition's side.
    pub fn terms_of(&self, n: Name) -> Vec<Term> {
        self.terms(|t| t.name() == Some(n))
    }

    /// The terms the state names that satisfy `keep`.
    fn terms(&self, keep: impl Fn(Term) -> bool) -> Vec<Term> {
        let mut out: Vec<Term> = Vec::new();
        let mut note = |t: Term| {
            if keep(t) && !out.contains(&t) {
                out.push(t);
            }
        };
        let lins = (self.facts.iter().chain(self.ne.iter()))
            .chain(self.defs.values())
            .chain((self.conds.values()).flat_map(|s| s.0.iter().chain(&s.1).map(Fact::lin)))
            .chain(self.elems.values().flatten());
        for l in lins {
            l.terms.iter().for_each(|(t, _)| note(*t));
        }
        self.defs.keys().for_each(|d| note(*d));
        out
    }

    /// Forgets the length of every element array ([`Term::Inner`]). A row
    /// that may write an element may replace or resize one, and a borrow of
    /// the array it writes through reads the same elements.
    pub fn forget_inner(&mut self) {
        for t in self.terms(|t| matches!(t, Term::Inner(..))) {
            self.forget(t);
        }
    }

    /// Forgets everything about `n`, its lengths and its fields. A definition
    /// `d = s*x + rest` with `s` one or minus one first restates every fact
    /// about `x` through `d`, so nothing known is lost to an exact rename.
    pub fn kill(&mut self, n: Name) {
        let cols: BTreeSet<Term> = (self.defs.iter())
            .flat_map(|(d, v)| std::iter::once(*d).chain(v.terms.iter().map(|(t, _)| *t)))
            .filter(|t| {
                matches!(t, Term::Col(..) | Term::Field(..) | Term::Inner(..)) && t.reads(n)
            })
            .collect();
        for t in [Term::Val(n), Term::Len(n)].into_iter().chain(cols) {
            self.defs.remove(&t);
            self.restate(t);
        }
        self.conds.remove(&n);
        self.fresh.remove(&n);
        self.unbound(|t| t.mentions_name(n));
        self.drop_lins(|l| l.mentions_name(n));
        self.unsay(|l| l.mentions_name(n));
    }

    /// Drops each condition's facts that `stale` holds of: a side states the
    /// rest of what it stated, which still follows from its truth.
    fn unsay(&mut self, stale: impl Fn(&Lin) -> bool) {
        let said = |(a, b): &(Vec<Fact>, Vec<Fact>)| a.iter().chain(b).any(|f| stale(f.lin()));
        if !self.conds.values().any(|s| said(s)) {
            return;
        }
        for s in self.conds.get_mut().values_mut().filter(|s| said(s)) {
            let (a, b) = s.get_mut();
            a.retain(|f| !stale(f.lin()));
            b.retain(|f| !stale(f.lin()));
        }
    }

    /// Drops each fact, disequality and definition whose sum `stale` holds
    /// of.
    fn drop_lins(&mut self, stale: impl Fn(&Lin) -> bool) {
        self.facts.retain(|f| !stale(f));
        self.ne.retain(|d| !stale(d));
        self.defs.retain(|_, v| !stale(v));
    }

    /// Forgets `n` as [`State::kill`] does, keeping what the facts state
    /// without it. A definition through `n` with coefficient one or minus one
    /// restates them; otherwise each pair of facts with opposite signs on `n`
    /// sums to a fact without it (Fourier-Motzkin). A fact of a condition's
    /// true side sums the same way with each fact of the state, keeping a sum
    /// over a length, so `a && b` keeps the index bound `b`'s truth states
    /// through a name `b` bound. The false side keeps no sum; no check needed
    /// one.
    pub fn eliminate(&mut self, n: Name) {
        let mut sums = BTreeSet::new();
        let mut said: Vec<(Name, Lin)> = Vec::new();
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
            // A fact over a length, or a side that cannot hold.
            let kept =
                |l: &Lin| (l.terms.iter()).any(|(t, _)| t.is_length()) || (l.is_const() && l.c < 0);
            let mentioned = |s: &Sides| s.0.iter().any(|f| f.lin().mentions(t));
            if !self.conds.values().any(mentioned) {
                continue;
            }
            for (c, s) in self.conds.iter() {
                for f in &s.0 {
                    let Fact::Ge(l) = f else { continue };
                    let k = l.coef(t);
                    if k == 0 {
                        continue;
                    }
                    for p in through.iter().filter(|p| p.coef(t).signum() == -k.signum()) {
                        let (a, b) = (k.checked_abs(), p.coef(t).checked_abs());
                        let sum = a.zip(b).and_then(|(a, b)| l.scale(b)?.add(&p.scale(a)?));
                        if let Some(l) = sum.filter(kept) {
                            said.push((*c, l));
                        }
                    }
                }
            }
        }
        self.kill(n);
        for s in sums.iter().filter(|s| !s.mentions_name(n)) {
            self.assume(s);
        }
        for (c, l) in said.into_iter().filter(|(_, l)| !l.mentions_name(n)) {
            if let Some(s) = self.conds.get_mut().get_mut(&c) {
                s.get_mut().0.push(Fact::Ge(l));
            }
        }
    }

    /// Forgets everything about the one term `t`, restating what a definition
    /// through it knows as [`State::kill`] does.
    pub fn forget(&mut self, t: Term) {
        self.unbound(|l| l.mentions(t));
        self.defs.remove(&t);
        self.restate(t);
        self.drop_lins(|l| l.mentions(t));
        self.unsay(|l| l.mentions(t));
    }

    /// Moves the length `t` by an unknown amount in `lo..=hi`, with no upper
    /// bound when `hi` is `None`. A fact with `k * t` holds of the new length
    /// once `-min(k*lo, k*hi)` is added, so with no upper bound one with
    /// `k < 0` goes; a definition of or through the length becomes its two
    /// facts first.
    pub fn shift(&mut self, t: Term, lo: i64, hi: Option<i64>) {
        let (through, defs): (BTreeMap<_, _>, _) = (std::mem::take(self.defs.get_mut()))
            .into_iter()
            .partition(|(d, v)| *d == t || v.coef(t) != 0);
        *self.defs.get_mut() = defs;
        let mut facts = std::mem::take(self.facts.get_mut());
        for (d, v) in through {
            facts.extend(Lin::of(d).sub(&v));
            facts.extend(v.sub(&Lin::of(d)));
        }
        self.ne.retain(|d| !d.mentions(t));
        self.unsay(|l| l.mentions(t));
        let moved = |f: &Lin| {
            let k = f.coef(t);
            let least = match hi {
                Some(hi) => (k.checked_mul(lo).zip(k.checked_mul(hi))).map(|(a, b)| a.min(b)),
                None if k >= 0 => k.checked_mul(lo),
                None => None,
            }?;
            f.plus(least.checked_neg()?)
        };
        for f in facts {
            if let Some(f) = moved(&f) {
                self.assume(&f);
            }
        }
        if !self.elems.is_empty() {
            for ts in self.elems.get_mut().values_mut() {
                *ts = ts.iter().filter_map(moved).collect();
            }
        }
    }

    /// Drops each element template `stale` holds of.
    fn unbound(&mut self, stale: impl Fn(&Lin) -> bool) {
        if !self.elems.values().flatten().any(&stale) {
            return;
        }
        let elems = self.elems.get_mut();
        for ts in elems.values_mut() {
            ts.retain(|t| !stale(t));
        }
        elems.retain(|_, ts| !ts.is_empty());
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
        // A sum that does not name `x` is its own substitution.
        let sub = |l: &Lin| l.subst(x, &by);
        let moved = |l: Lin| if l.mentions(x) { sub(&l) } else { Some(l) };
        // A length's axioms outlive the length: they bound the renamed value.
        let facts = self.facts.get_mut();
        *facts = (std::mem::take(facts).into_iter())
            .chain(axioms([x].into_iter()))
            .filter_map(moved)
            .collect();
        if self.ne.iter().any(|d| d.mentions(x)) {
            let ne = self.ne.get_mut();
            *ne = (std::mem::take(ne).into_iter())
                .filter_map(|d| moved(d).and_then(canon))
                .filter(|d| !d.is_const())
                .collect();
        }
        if self.defs.values().any(|v| v.mentions(x)) {
            for v in self.defs.get_mut().values_mut().filter(|v| v.mentions(x)) {
                if let Some(n) = sub(v) {
                    *v = n;
                }
            }
        }
        let named =
            |(a, b): &(Vec<Fact>, Vec<Fact>)| a.iter().chain(b).any(|f| f.lin().mentions(x));
        if self.conds.values().any(|s| named(s)) {
            for s in self.conds.get_mut().values_mut().filter(|s| named(s)) {
                let (a, b) = s.get_mut();
                for f in (a.iter_mut().chain(b.iter_mut())).filter(|f| f.lin().mentions(x)) {
                    if let Some(n) = f.map(sub) {
                        *f = n;
                    }
                }
            }
        }
        if self.elems.values().flatten().any(|t| t.mentions(x)) {
            for ts in self.elems.get_mut().values_mut() {
                if ts.iter().any(|t| t.mentions(x)) {
                    *ts = ts.iter().filter_map(sub).collect();
                }
            }
        }
    }

    /// Answers whether `goal >= 0` holds with at most two premises, or with
    /// one premise and a length axiom for each length the rest names.
    pub fn ge0(&self, goal: &Lin) -> Option<Cert> {
        self.prover().ge0(goal)
    }

    /// The state's premises indexed once, for many queries.
    pub fn prover(&self) -> Prover<'_> {
        let mut lengths: Vec<Term> = (self.facts.iter().flat_map(|f| &f.terms))
            .map(|(t, _)| *t)
            .filter(|t| t.is_length())
            .collect();
        lengths.sort_unstable();
        lengths.dedup();
        let mut best: HashMap<&[(Term, i64)], &Lin> = HashMap::new();
        for p in self.facts.iter() {
            let e = best.entry(p.terms.as_slice()).or_insert(p);
            if p.c < e.c {
                *e = p;
            }
        }
        Prover {
            st: self,
            lengths,
            best,
        }
    }

    /// The facts and disequalities every live state of `sts` proves, the
    /// definitions they all share, and per Bool name the facts each side gives
    /// on every path ([`State::when`]). A live state that is the only one is
    /// the join, taken out of `sts`.
    pub fn join(sts: &mut [State]) -> State {
        if sts.iter().filter(|s| !s.dead).count() < 2 {
            return (sts.iter_mut().find(|s| !s.dead)).map_or_else(State::dead, std::mem::take);
        }
        let live: Vec<&State> = sts.iter().filter(|s| !s.dead).collect();
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
        // Sides every live state shares are the join's: `when` answers
        // each side as given.
        let conds: Vec<_> = (names.into_iter())
            .map(|n| {
                let first = &live[0].conds[&n];
                if live.iter().all(|s| s.conds[&n] == *first) {
                    return (n, first.clone());
                }
                let t = State::when(&live, n, &known, |c| &c.0);
                let f = State::when(&live, n, &known, |c| &c.1);
                (n, Sides::from((t, f)))
            })
            .collect();
        for (n, side) in conds
            .into_iter()
            .filter(|(_, s)| !s.0.is_empty() || !s.1.is_empty())
        {
            out.conds.insert(n, side);
        }
        out
    }

    /// The facts, disequalities and definitions of [`State::join`], without
    /// conditions.
    fn common(live: &[&State]) -> State {
        let Some(first) = live.first() else {
            return State::dead();
        };
        let mut out = State::default();
        for n in (first.fresh.iter()).filter(|n| live.iter().all(|s| s.fresh.contains(n))) {
            out.fresh.insert(*n);
        }
        for (n, ts) in first.elems.iter() {
            let all: BTreeSet<Lin> = (ts.iter())
                .filter(|t| {
                    live.iter()
                        .all(|s| s.elems.get(n).is_some_and(|x| x.contains(t)))
                })
                .cloned()
                .collect();
            if !all.is_empty() {
                out.elems.insert(*n, all);
            }
        }
        if live.iter().all(|s| s.defs == first.defs) {
            out.defs = first.defs.clone();
        } else {
            for (t, v) in first.defs.iter() {
                if live.iter().all(|s| s.defs.get(t) == Some(v)) {
                    out.defs.insert(*t, v.clone());
                }
            }
        }
        // Live states with equal facts, disequalities and definitions join
        // to them: each fact proves itself, names no defined term and is no
        // constant, which `assume` would drop.
        let same = |s: &&State| s.facts == first.facts && s.ne == first.ne && s.defs == first.defs;
        if live.iter().all(same) && !first.facts.iter().any(Lin::is_const) {
            debug_assert!((first.facts.iter().chain(first.ne.iter()))
                .all(|l| matches!(first.norm(l), Some(Cow::Borrowed(_)))));
            out.facts = first.facts.clone();
            out.ne = first.ne.clone();
            return out;
        }
        let mut cands: BTreeSet<Lin> = BTreeSet::new();
        for s in live {
            cands.extend(s.facts.iter().cloned());
            cands.extend(s.unshared(&out));
        }
        let provers: Vec<Prover> = live.iter().map(|s| s.prover()).collect();
        for c in cands {
            if provers.iter().all(|p| p.proves(&c)) {
                out.assume(&c);
            }
        }
        let ne: BTreeSet<&Lin> = live.iter().flat_map(|s| s.ne.iter()).collect();
        for d in ne {
            if provers.iter().all(|p| p.differs(d)) {
                if let Some(d) = out.norm(d).map(Cow::into_owned).and_then(canon) {
                    out.ne.insert(d);
                }
            }
        }
        // A term some state defines as a constant takes one of the values
        // the states allow; a value between them no state allows is a
        // disequality: `w = 0` joined with `w = 2` keeps `w != 1`.
        let consts: BTreeSet<Term> = (live.iter().flat_map(|s| s.defs.iter()))
            .filter(|(t, v)| v.is_const() && !out.defs.contains_key(t))
            .map(|(t, _)| *t)
            .collect();
        for t in consts {
            let mut all = BTreeSet::new();
            if !live.iter().all(|s| s.values(t, &mut all).is_some()) {
                // Too many values to list: the least and greatest bound any
                // state shows, `k = 0` joined with `k = -e`, `e >= -1074`.
                let v = Lin::of(t);
                let neg = Lin {
                    terms: vec![(t, -1)],
                    c: 0,
                };
                let bounds = |p: &Prover| Some((p.least(&v)?, p.least(&neg)?));
                if let Some(b) = provers.iter().map(bounds).collect::<Option<Vec<_>>>() {
                    let lo = b.iter().map(|(l, _)| *l).min();
                    let hi = b.iter().map(|(_, h)| *h).min();
                    let at = |l: &Lin, m: Option<i64>| l.plus(m?.checked_neg()?);
                    at(&v, lo)
                        .iter()
                        .chain(&at(&neg, hi))
                        .for_each(|f| out.assume(f));
                }
                continue;
            }
            let (Some(&lo), Some(&hi)) = (all.first(), all.last()) else {
                continue;
            };
            // Constants far apart leave no gap worth a disequality, and
            // `k = 1` joined with `k = 2000000000` would list two billion.
            if hi.checked_sub(lo).is_none_or(|d| d >= VALUES) {
                continue;
            }
            for v in (lo..hi).filter(|v| !all.contains(v)) {
                if let Some(d) = Lin::of(t).plus(-v).and_then(canon) {
                    out.ne.insert(d);
                }
            }
        }
        out
    }

    /// Adds to `into` the values `t` may take: its constant, or the integers
    /// its bounds `t - lo >= 0` and `hi - t >= 0` leave less its
    /// disequalities. `None` when they are not known or more than [`VALUES`].
    fn values(&self, t: Term, into: &mut BTreeSet<i64>) -> Option<()> {
        if let Some(d) = self.defs.get(&t) {
            return d.is_const().then(|| {
                into.insert(d.c);
            });
        }
        let bound = |k: i64| self.facts.iter().filter(move |f| f.terms == [(t, k)]);
        let lo = bound(1).filter_map(|f| f.c.checked_neg()).max()?;
        let hi = bound(-1).map(|f| f.c).min()?;
        if hi.checked_sub(lo)? >= VALUES {
            return None;
        }
        let gone = |v: i64| {
            !self.ne.is_empty()
                && (Lin::of(t).plus(-v).and_then(canon)).is_some_and(|d| self.ne.contains(&d))
        };
        into.extend((lo..=hi).filter(|v| !gone(*v)));
        Some(())
    }

    /// Each definition of `self` that `out` lacks, as its two facts.
    pub fn unshared<'a>(&'a self, out: &'a State) -> impl Iterator<Item = Lin> + 'a {
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
        side: impl Fn(&(Vec<Fact>, Vec<Fact>)) -> &Vec<Fact>,
    ) -> Vec<Fact> {
        let given: Vec<&Vec<Fact>> = live.iter().map(|s| side(&s.conds[&n])).collect();
        if given.iter().all(|g| *g == given[0]) {
            return given[0].clone();
        }
        let mut under: Vec<State> = (live.iter().zip(&given))
            .map(|(s, g)| {
                let mut s = State {
                    facts: s.facts.clone(),
                    ne: s.ne.clone(),
                    defs: s.defs.clone(),
                    ..State::default()
                };
                g.iter().for_each(|f| s.add(f));
                s
            })
            .filter(|s| !s.dead)
            .collect();
        let j = match under.len() {
            1 => under.swap_remove(0),
            _ => State::common(&under.iter().collect::<Vec<_>>()),
        };
        if j.dead {
            return vec![Fact::Ge(Lin::k(-1))];
        }
        (j.facts.iter().cloned())
            .chain(j.unshared(known.st))
            .filter(|c| !known.proves(c))
            .map(Fact::Ge)
            .chain((j.ne.iter().filter(|d| !known.differs(d))).map(|d| Fact::Ne(d.clone())))
            .collect()
    }
}

/// A state's facts and length axioms, with the strongest premise per term map
/// (the least constant).
pub struct Prover<'a> {
    st: &'a State,
    /// The lengths the facts name, sorted. The premises are `st.facts`, then
    /// the axioms of these.
    lengths: Vec<Term>,
    /// For each term list, the fact with the least constant.
    best: HashMap<&'a [(Term, i64)], &'a Lin>,
}

/// A premise of a search: a fact of the state, or a length axiom held
/// inline ([`axiom`]).
#[derive(Clone, Copy)]
enum Prem<'a> {
    Fact(&'a Lin),
    Axiom([(Term, i64); 1], i64),
}

impl Prem<'_> {
    fn terms(&self) -> &[(Term, i64)] {
        match self {
            Prem::Fact(l) => &l.terms,
            Prem::Axiom(t, _) => t,
        }
    }

    fn c(&self) -> i64 {
        match self {
            Prem::Fact(l) => l.c,
            Prem::Axiom(_, c) => *c,
        }
    }

    fn lin(&self) -> Lin {
        Lin {
            terms: self.terms().to_vec(),
            c: self.c(),
        }
    }
}

/// The axiom of the length `t` with coefficient `k`, one or minus one:
/// `t >= 0`, or `LEN_MAX - t >= 0`.
fn axiom(t: Term, k: i64) -> Prem<'static> {
    Prem::Axiom([(t, k)], if k < 0 { LEN_MAX } else { 0 })
}

/// The axiom whose terms are `terms`, when they are one length `has` holds
/// with coefficient one or minus one.
fn axiom_on(terms: &[(Term, i64)], has: impl Fn(Term) -> bool) -> Option<Prem<'static>> {
    match *terms {
        [(t, k)] if t.is_length() && matches!(k, 1 | -1) && has(t) => Some(axiom(t, k)),
        _ => None,
    }
}

impl<'a> Prover<'a> {
    /// Whether the state proves `l != 0`: it holds it ([`State::differs`]),
    /// or a bound keeps `l` above or below zero.
    pub fn differs(&self, l: &Lin) -> bool {
        let off = |e: Option<Lin>| (e.and_then(|e| e.plus(-1))).is_some_and(|e| self.proves(&e));
        self.st.differs(l) || off(Some(l.clone())) || off(l.scale(-1))
    }

    /// The premises of a search for `g`: the facts, the axioms of their
    /// lengths, then those of `g`'s own lengths, which may appear in no fact.
    fn premises<'g>(&'g self, g: &'g Lin) -> impl Iterator<Item = Prem<'a>> + 'g {
        let axioms = |t: &Term| [axiom(*t, 1), axiom(*t, -1)];
        (self.st.facts.iter().map(Prem::Fact))
            .chain(self.lengths.iter().flat_map(axioms))
            .chain(
                (g.terms.iter().map(|(t, _)| t))
                    .filter(|t| t.is_length())
                    .flat_map(axioms),
            )
    }

    /// [`Prover::ge0`] with one premise more: a premise that names the
    /// goal's first term, then the rest by `ge0`. Some premise of a sum names
    /// each of the goal's terms, so no chain of three is missed for that
    /// choice. The walk asks it only of a check's goal and of a shrink's
    /// receiver being non-empty: asked at every join and loop head too, it
    /// cost `elide: summaries` 7.6 times its allocations.
    pub fn ge0_deep(&self, goal: &Lin) -> Option<Cert> {
        if let Some(c) = self.ge0(goal) {
            return Some(c);
        }
        let g = self.st.norm(goal)?;
        let (first, _) = *g.terms.first()?;
        let named = self
            .premises(&g)
            .filter(|p| p.terms().iter().any(|(t, _)| *t == first));
        for p in named {
            let mut d = Lin::k(0);
            if g.add_scaled_into(p.terms(), p.c(), -1, &mut d).is_none() {
                continue;
            }
            if let Some(Cert::Sum { uses, slack }) = self.ge0(&d) {
                let uses = std::iter::once((1, p.lin())).chain(uses).collect();
                return Some(Cert::Sum { uses, slack });
            }
        }
        None
    }

    /// The least value of `l` the state proves, when it is above `-EXACT`:
    /// the slack of `l + EXACT >= 0`, checked again as a goal of its own.
    pub fn least(&self, l: &Lin) -> Option<i64> {
        let Cert::Sum { slack, .. } = self.ge0(&l.plus(EXACT)?)? else {
            return None;
        };
        let m = slack.checked_sub(EXACT)?;
        let g = l.plus(m.checked_neg()?)?;
        self.ge0(&g)
            .is_some_and(|c| c.verify(self.st, &g))
            .then_some(m)
    }

    /// [`State::ge0`], over the index.
    pub fn ge0(&self, goal: &Lin) -> Option<Cert> {
        if self.st.dead {
            return Some(Cert::Dead);
        }
        let sum = |uses: &[(i64, Prem)], slack| Cert::Sum {
            uses: uses.iter().map(|(k, p)| (*k, p.lin())).collect(),
            slack,
        };
        Some(match self.prove(goal, sum)? {
            (1, sum) => sum,
            (by, inner) => Cert::Scaled {
                by,
                inner: Box::new(inner),
            },
        })
    }

    /// Whether [`Prover::ge0`] proves `goal`, without its certificate.
    pub fn proves(&self, goal: &Lin) -> bool {
        self.st.dead || self.prove(goal, |_, _| ()).is_some()
    }

    /// The search of [`Prover::ge0`]: `done` of the premises and the slack it
    /// found, with the factor it divided the goal by, one when none. A goal
    /// whose coefficients share a factor is also asked divided by it:
    /// `2k - 1 >= 0` as `k - 1 >= 0`.
    fn prove<R>(
        &self,
        goal: &Lin,
        done: impl Fn(&[(i64, Prem<'a>)], i64) -> R,
    ) -> Option<(i64, R)> {
        if let Some(r) = self.search(goal, &done) {
            return Some((1, r));
        }
        let g = self.st.norm(goal)?;
        let by = (g.terms.iter()).fold(0, |d, (_, k)| gcd(d, k.unsigned_abs()));
        let by = i64::try_from(by).ok().filter(|b| *b > 1)?;
        let terms = g.terms.iter().map(|&(t, k)| (t, k / by)).collect();
        let divided = Lin {
            terms,
            c: g.c.div_euclid(by),
        };
        Some((by, self.search(&divided, &done)?))
    }

    /// Proves `goal >= 0` from at most two premises, or from one premise and
    /// a length axiom for each length the rest names, and answers `done` of
    /// the premises, each taken a whole number of times, and the slack.
    fn search<R>(&self, goal: &Lin, done: impl Fn(&[(i64, Prem<'a>)], i64) -> R) -> Option<R> {
        let g = self.st.norm(goal)?;
        if g.is_const() {
            return (g.c >= 0).then(|| done(&[], g.c));
        }
        let one = |terms: &[(Term, i64)], c: i64| {
            let fact = self.best.get(terms).map(|p| Prem::Fact(p));
            let indexed = axiom_on(terms, |t| self.lengths.binary_search(&t).is_ok());
            // The premise with the least constant, the first on a tie.
            let indexed = fact
                .into_iter()
                .chain(indexed)
                .reduce(|m, p| if p.c() < m.c() { p } else { m });
            indexed
                .into_iter()
                .chain(axiom_on(terms, |t| g.coef(t) != 0))
                .find(|p| p.c() <= c)
        };
        if let Some(p) = one(&g.terms, g.c) {
            return Some(done(&[(1, p)], g.c - p.c()));
        }
        let length = |(t, k): &(Term, i64)| t.is_length() && matches!(k, 1 | -1);
        let mut d = Lin::k(0);
        for p in self.premises(&g) {
            if g.add_scaled_into(p.terms(), p.c(), -1, &mut d).is_none() {
                continue;
            }
            if let Some(q) = one(&d.terms, d.c) {
                return Some(done(&[(1, p), (1, q)], d.c - q.c()));
            }
            // A rest `k*t + c` takes the premise on `t` `|k|` times:
            // `off + 4t` from `off >= 0` and four times `t >= 0`.
            if let [(t, k)] = d.terms[..] {
                let by = k.checked_abs().filter(|b| *b > 1);
                let q = by.and_then(|by| one(&[(t, k / by)], d.c.div_euclid(by)));
                let slack = q
                    .zip(by)
                    .and_then(|(q, by)| d.c.checked_sub(q.c().checked_mul(by)?));
                if let (Some(q), Some(by), Some(slack)) = (q, by, slack) {
                    return Some(done(&[(1, p), (by, q)], slack));
                }
            }
            // A rest over lengths alone, each with coefficient one or minus
            // one, takes one axiom per length: `i <= a - b` gives
            // `i <= LEN_MAX`.
            if !d.terms.iter().all(length) {
                continue;
            }
            let rest = d.terms.iter().map(|&(t, k)| axiom(t, k));
            let slack = rest.clone().try_fold(d.c, |c, a| c.checked_sub(a.c()));
            if let Some(slack) = slack.filter(|s| *s >= 0) {
                let uses: Vec<_> = std::iter::once(p).chain(rest).map(|u| (1, u)).collect();
                return Some(done(&uses, slack));
            }
        }
        None
    }
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// `l` or `-l`, whichever has a positive first coefficient: the one form a
/// state stores `l != 0` in.
fn canon(l: Lin) -> Option<Lin> {
    match l.terms.first() {
        Some((_, k)) if *k < 0 => l.scale(-1),
        _ => Some(l),
    }
}

/// `len >= 0` and `LEN_MAX - len >= 0` for every length among `terms`.
fn axioms(terms: impl Iterator<Item = Term>) -> Vec<Lin> {
    (terms.filter(|t| t.is_length()))
        .flat_map(|t| [axiom(t, 1), axiom(t, -1)])
        .map(|p| p.lin())
        .collect()
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
    fn a_fact_over_two_lengths_bounds_its_value_by_their_axioms() {
        let mut st = State::default();
        // len(a) - len(b) - i >= 0
        let (a, b) = (Lin::of(Term::Len(Name(1))), Lin::of(Term::Len(Name(2))));
        st.assume(&a.sub(&b).and_then(|d| d.sub(&v(0))).unwrap());
        let goal = Lin::k(LEN_MAX).sub(&v(0)).unwrap();
        let cert = st.ge0(&goal).expect("i <= len(a) <= LEN_MAX");
        assert!(cert.verify(&st, &goal));
        assert!(st.ge0(&goal.plus(-1).unwrap()).is_none());
    }

    #[test]
    fn a_certificate_with_a_premise_the_state_lacks_fails() {
        let st = State::default();
        let cert = Cert::Sum {
            uses: vec![(1, v(0))],
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
        st.shift(Term::Len(Name(1)), -1, Some(0));
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
    fn an_elimination_keeps_a_conditions_sum_with_the_state() {
        let mut st = State::default();
        // Where c is true, len(a) - w - 1 >= 0; with w >= 0, len(a) - 1 >= 0
        // once w is gone.
        let len = Lin::of(Term::Len(Name(0)));
        st.assume(&v(1));
        let side = vec![Fact::Ge(len.sub(&v(1)).unwrap().plus(-1).unwrap())];
        st.conds.insert(Name(2), Sides::from((side, Vec::new())));
        st.eliminate(Name(1));
        assert_eq!(st.conds[&Name(2)].0, [Fact::Ge(len.plus(-1).unwrap())]);
    }

    #[test]
    fn a_join_keeps_only_what_every_branch_proves() {
        let (mut a, mut b) = (State::default(), State::default());
        a.assume(&v(0).plus(-2).unwrap());
        b.assume(&v(0).plus(-1).unwrap());
        let j = State::join(&mut [a, b]);
        assert!(j.ge0(&v(0).plus(-1).unwrap()).is_some());
        assert!(j.ge0(&v(0).plus(-2).unwrap()).is_none());
    }

    #[test]
    fn a_join_of_constants_keeps_the_values_between_out() {
        let at = |c: i64| {
            let mut s = State::default();
            s.define(Term::Val(Name(0)), &Lin::k(c));
            s
        };
        let inner = State::join(&mut [at(0), at(4)]);
        let mut j = State::join(&mut [at(2), at(3), inner]);
        assert!(j.ge0(&Lin::k(4).sub(&v(0)).unwrap()).is_some());
        j.differ(&v(0));
        assert!(j.ge0(&v(0).plus(-2).unwrap()).is_some());
        assert!(j.ge0(&v(0).plus(-3).unwrap()).is_none());
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
