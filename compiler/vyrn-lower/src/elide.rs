//! Proves check rows ([`crate::check`]) that cannot fail, from what one body
//! states about its own names ([`crate::facts`]).
//!
//! [`decide`] walks a body's rows forward. A `let` or a store of an integer
//! defines its name; a comparison remembers the facts its truth and falsehood
//! give, a Bool copy and a join carry them, and an `if` on it assumes them; a
//! builtin's row states how it moves its receiver's length, and any other
//! `modify` or `consume` argument that is not a scalar forgets its name. After
//! a check row the path has its guard, since it traps otherwise. A loop's head keeps the candidate facts that hold at
//! entry and after every turn (Houdini): each round drops at least one
//! candidate or stops, so the candidate count bounds the rounds.
//!
//! A direct call's result has the facts its callee's [`Summary`] states,
//! over the call's arguments ([`summaries`]). Any other call's result is a
//! fresh value. A body that only direct call rows enter starts with the facts
//! every such row proves of its arguments. Every body starts with its
//! parameters' `where` clauses ([`Body::assumes`]), which every call row
//! checks; a call's clause check is a row like any other.
//!
//! The walk knows nothing about a global, an element, or a field other than an
//! integer field of a `read` parameter: each read of one is a fresh value with
//! its type's range. An exact sum is an `Int64` sum that provably stays in
//! `-2^62..=2^62`. A row is proved only with a certificate that
//! [`crate::facts::Cert::verify`] accepts, or a divisor the state holds
//! unequal to zero.
//!
//! One postulate: a live read borrow's source is not written, by the kernel's
//! exclusivity judgment, so a borrow's length, and a field of a `read`
//! parameter, changes only where the walk sees the borrow itself written.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write;
use std::sync::{Arc, Mutex};

use vyrn_frontend::ast::{BinOp, Capability, FnId, Type, TypeDecl, UnOp};
use vyrn_frontend::effects::Effect;
use vyrn_frontend::prelude::{self, Length};
use vyrn_frontend::prim::Cmp;

use crate::facts::{Fact, Lin, State, Term};
use vyrn_frontend::core::check::{Atom, Check, Guard, Operand, Site, Verdict, Why};
use vyrn_frontend::core::{
    rows, Arg, Body, BorrowKind, Callee, Ctor, Lit, Name, Op, Place, Rhs, St, Target, Val,
};
use vyrn_frontend::par::in_parallel;

/// The bound an exact `Int64` sum must provably stay within: `2^62`, so no
/// premise or goal of the prover itself leaves `i64`.
const EXACT: i64 = 1 << 62;

/// Marks every check row of `body`, and of each lambda body it holds, that
/// cannot fail [`Verdict::Proved`], with the callees' facts `sums` states.
pub fn decide(body: &mut Body, decls: &HashMap<String, TypeDecl>, sums: &Summaries) {
    decide_in(body, decls, sums.into());
}

fn decide_in(body: &mut Body, decls: &HashMap<String, TypeDecl>, sums: View<'_>) {
    for l in &mut body.lambdas {
        decide_in(l, decls, sums);
    }
    walk(body, decls, sums);
}

/// A group's rule check that fails wherever it runs: its site, the field the
/// facts prove longer, and the field they prove shorter, as the rule names
/// them.
pub type Refuted = (Site, String, String);

/// Decides the check rows of the one frame `body` as [`decide`] does, without
/// its lambdas and without the callees' facts: a refusal must not depend on
/// another body's summary, so `vyrn check` and the emitters refuse alike.
/// Answers each rule check that ends a group ([`crate::typed::groups`]) where
/// the facts prove one field of an equal pair longer than the other, on every
/// live path. A dead state proves every goal, so it refutes none.
pub fn refuted(body: &mut Body, decls: &HashMap<String, TypeDecl>) -> Vec<Refuted> {
    walk(body, decls, (&Summaries::default()).into())
}

fn walk(body: &mut Body, decls: &HashMap<String, TypeDecl>, sums: View<'_>) -> Vec<Refuted> {
    if !any_check(&body.stmts) {
        return Vec::new();
    }
    let mut stmts = std::mem::take(&mut body.stmts);
    let none = BTreeSet::new();
    let pre = body
        .id
        .and_then(|f| sums.get(f))
        .map_or(&none, |(_, s)| &s.pre);
    let mut w = Walk::new(body, decls, sums, &stmts, None);
    let st = w.entry(pre);
    w.block(st, &mut stmts);
    let refuted = w.refuted;
    body.stmts = stmts;
    refuted
}

/// What every entry and every return of a body state, over its interface:
/// in a fact, `Name(0)` is the result and `Name(k + 1)` is parameter `k`; a
/// [`Term::Val`] is an integer's value and a [`Term::Len`] an array's or
/// String's length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// Per parameter, its kind; a call whose arguments differ states nothing.
    params: Vec<Kind>,
    /// Facts over the parameters that every call row proves of its
    /// arguments before the call. Empty for a body anything else may enter.
    pre: BTreeSet<Lin>,
    /// Facts every return proves, whatever the arguments. A fact names only
    /// parameters the body never writes, so it holds of the arguments as the
    /// caller passed them.
    post: BTreeSet<Lin>,
}

/// Each summarized body's [`Summary`], keyed by its row in the World's
/// function table, never by a name.
#[derive(Debug, Default)]
pub struct Summaries {
    at: HashMap<FnId, usize>,
    values: Vec<Summary>,
    /// Bodies [`summaries`] decided as [`decide`] decides them, each served
    /// once ([`Summaries::decided`]).
    decided: HashMap<FnId, Mutex<Option<Body>>>,
    pairs: Pairs,
}

/// What [`invariants`] reads besides the bodies, made once per program.
#[derive(Debug, Default)]
pub struct Records {
    /// The declared types whose values the host or a builtin makes: those a
    /// builtin's or an `extern` function's signature names, the prelude's,
    /// and every declared type they hold.
    host: BTreeSet<String>,
    /// The declaration a `modify` parameter's type names, by the function's
    /// [`FnId`] index and the parameter's.
    params: Arc<BTreeMap<(usize, usize), String>>,
}

impl Records {
    /// The records of `program`, whose host-made types are those `roots`
    /// name and every declared type they hold.
    pub fn new<'t>(
        program: &vyrn_frontend::ast::Program,
        roots: impl IntoIterator<Item = &'t Type>,
        decls: &'t HashMap<String, TypeDecl>,
    ) -> Records {
        let mut host = BTreeSet::new();
        let mut stack: Vec<&Type> = roots.into_iter().collect();
        while let Some(t) = stack.pop() {
            vyrn_frontend::types::walk_type(t, &mut |x| {
                if let Type::Named(n) | Type::App(n, _) = x {
                    if let Some(d) = decls.get(n).filter(|_| !host.contains(n)) {
                        host.insert(n.clone());
                        stack.push(&d.base);
                    }
                }
            });
        }
        let params = (program.functions.iter().enumerate())
            .flat_map(|(g, f)| {
                (f.params.iter().enumerate()).filter_map(move |(k, p)| {
                    match (&p.capability, &p.ty) {
                        (Capability::Modify, Type::Named(d) | Type::App(d, _)) => {
                            Some(((g, k), d.clone()))
                        }
                        _ => None,
                    }
                })
            })
            .collect();
        Records {
            host,
            params: Arc::new(params),
        }
    }
}

/// Pairs of array fields, by name, whose lengths are equal in every record
/// value that has both ([`invariants`]).
#[derive(Debug, Default)]
pub struct Pairs {
    /// Each pair's first field name, to the names after it it pairs with.
    of: BTreeMap<String, BTreeSet<String>>,
    /// Per record declaration that has both fields of a pair, those pairs.
    by_decl: HashMap<String, Vec<(String, String)>>,
    /// [`Records::params`].
    params: Arc<BTreeMap<(usize, usize), String>>,
}

impl Pairs {
    fn new(
        of: BTreeMap<String, BTreeSet<String>>,
        decls: &HashMap<String, TypeDecl>,
        params: &Arc<BTreeMap<(usize, usize), String>>,
    ) -> Pairs {
        let mut pairs = Pairs {
            of,
            by_decl: HashMap::new(),
            params: params.clone(),
        };
        if pairs.of.is_empty() {
            return pairs;
        }
        for name in decls.keys() {
            let ty = Type::Named(name.clone());
            let Some(fields) = vyrn_frontend::types::record_fields(&ty, decls) else {
                continue;
            };
            let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
            let within: Vec<(String, String)> = (pairs.within(&names).into_iter())
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect();
            if !within.is_empty() {
                pairs.by_decl.insert(name.clone(), within);
            }
        }
        pairs
    }

    fn is_empty(&self) -> bool {
        self.of.is_empty()
    }

    /// The pairs of the declared type of `modify` parameter `k` of `g`.
    fn param(&self, g: FnId, k: usize) -> Option<&[(String, String)]> {
        let d = self.params.get(&(g.index(), k))?;
        Some(self.by_decl.get(d).map_or(&[], Vec::as_slice))
    }

    /// The pairs a record of type `ty` has both fields of.
    fn of_type(&self, ty: &Type, decls: &HashMap<String, TypeDecl>) -> Vec<(&str, &str)> {
        if self.is_empty() {
            return Vec::new();
        }
        match ty {
            Type::Named(d) | Type::App(d, _) => (self.by_decl.get(d).into_iter().flatten())
                .map(|(a, b)| (a.as_str(), b.as_str()))
                .collect(),
            _ => match vyrn_frontend::types::record_fields(ty, decls) {
                Some(fs) => self.within(&fs.iter().map(|f| f.name.as_str()).collect::<Vec<_>>()),
                None => Vec::new(),
            },
        }
    }

    /// The pairs with both fields among `fields`.
    fn within(&self, fields: &[impl AsRef<str>]) -> Vec<(&str, &str)> {
        let mut out = Vec::new();
        for f in fields {
            let Some((a, bs)) = self.of.get_key_value(f.as_ref()) else {
                continue;
            };
            for g in fields {
                if let Some(b) = bs.get(g.as_ref()) {
                    out.push((a.as_str(), b.as_str()));
                }
            }
        }
        out
    }
}

impl Summaries {
    /// The body of `f` as [`decide`] decides it, when [`summaries`] decided
    /// it and no caller took it before.
    pub fn decided(&self, f: FnId) -> Option<Body> {
        self.decided.get(&f)?.lock().ok()?.take()
    }
}

/// [`Summaries`] borrowed: the solver's values while one body is walked.
#[derive(Clone, Copy)]
struct View<'a> {
    at: &'a HashMap<FnId, usize>,
    values: &'a [Summary],
    pairs: &'a Pairs,
}

impl View<'_> {
    /// The summary of `f`, with its index.
    fn get(&self, f: FnId) -> Option<(usize, &Summary)> {
        let i = *self.at.get(&f)?;
        Some((i, self.values.get(i)?))
    }

    /// Whether `f` states a fact of its result.
    fn post(&self, f: FnId) -> bool {
        self.get(f).is_some_and(|(_, s)| !s.post.is_empty())
    }

    /// The callee of `s`, a direct call to a body with entry facts, with the
    /// call's arguments.
    fn pre<'s>(&self, s: &'s St) -> Option<(usize, &Summary, &'s [(Arg, Capability)])> {
        let (St::Let(_, rhs) | St::Do { rhs, .. }) = s else {
            return None;
        };
        let Rhs::Call { args, .. } = rhs else {
            return None;
        };
        let (i, sum) = self.get(direct_call(rhs)?)?;
        (!sum.pre.is_empty()).then_some((i, sum, args.as_slice()))
    }
}

impl<'a> From<&'a Summaries> for View<'a> {
    fn from(s: &'a Summaries) -> View<'a> {
        View {
            at: &s.at,
            values: &s.values,
            pairs: &s.pairs,
        }
    }
}

/// The facts every return and every entry of each body in `bodies` state.
///
/// A body's `post` is the greatest fixpoint ([`crate::fixpoint::descend`])
/// from every candidate of [`templates`], in which each body's returns prove
/// its facts under its callees' facts. Only the bodies a check can read are
/// solved: each callee of a direct call whose result can reach a check
/// ([`seeds`]), and each callee a solved body with facts left took facts
/// from. Any other body keeps no `post`.
///
/// Then a body of `closed` that [`entered`] keeps takes the candidates of
/// [`entry_templates`] as its `pre`, less each fact some call row to it does
/// not prove, its caller walked with every `post` and without its own `pre`
/// ([`entries`]). Every caller is walked once, so no `pre` depends on another.
///
/// The result does not depend on the order of `bodies`. Soundness rests on
/// two postulates: a [`Callee::Fn`] row runs the body `bodies` holds under
/// its row, and `bodies` holds no body for a row two bodies share or for a
/// generic function, whose instances run other bodies; and only a
/// [`Callee::Fn`] row of a body in `bodies`, or a row that spells its name,
/// enters a body of `closed`.
///
/// Every walk assumes the record [`Pairs`] that [`invariants`] keeps from
/// `records`; `None` when a body that runs may be missing from `bodies`, and
/// then no pair holds.
pub fn summaries<'a>(
    bodies: impl Iterator<Item = (FnId, &'a Body)>,
    decls: &HashMap<String, TypeDecl>,
    closed: &HashSet<FnId>,
    records: Option<&Records>,
) -> Summaries {
    let mut bodies: Vec<(FnId, &Body)> = bodies.collect();
    bodies.sort_by_key(|(f, _)| f.index());
    // A body's name count weighs its work.
    let weigh = |(_, b): &(FnId, &Body)| b.names.len();
    let (values, scans): (Vec<Summary>, Vec<Option<Scan>>) = in_parallel(
        &bodies,
        weigh,
        || (),
        |(), (_, b)| (templates(b, decls), records.map(|_| scan(b, decls))),
    )
    .into_iter()
    .unzip();
    let scans: Option<Vec<Scan>> = scans.into_iter().collect();
    let pairs = match (records, scans) {
        (Some(r), Some(scans)) => invariants(&bodies, &scans, decls, r),
        _ => Pairs::default(),
    };
    let mut at: HashMap<FnId, usize> = (0..).zip(&bodies).map(|(i, (f, _))| (*f, i)).collect();
    let view = View {
        at: &at,
        values: &values,
        pairs: &pairs,
    };
    let start: BTreeSet<usize> =
        in_parallel(&bodies, weigh, || (), |(), (_, b)| seeds(b, decls, view))
            .into_iter()
            .flatten()
            .collect();
    let mut seen = vec![false; bodies.len()];
    start.iter().for_each(|&i| seen[i] = true);
    // Per body, the bodies whose facts its facts went into: visited again
    // when it loses one. Every reader of a lowered callee is revisited: a
    // reader that registers after its callee's first visit may have read a
    // value that an earlier update of the same round has since lowered.
    let mut readers = vec![BTreeSet::new(); bodies.len()];
    let walk = |i: usize, values: &[Summary]| {
        let view = View {
            at: &at,
            values,
            pairs: &pairs,
        };
        returns(bodies[i].1, decls, view, &values[i])
    };
    let weight = |i: usize| bodies[i].1.names.len();
    let mut values =
        crate::fixpoint::descend(values, start, weight, walk, |i, (kept, read), values| {
            let mut next = Vec::new();
            if kept.len() < values[i].post.len() {
                next.extend(readers[i].iter().copied());
            }
            values[i].post = kept;
            // A body with no facts left reads no callee's.
            if !values[i].post.is_empty() {
                for j in read {
                    let new = readers[j].insert(i);
                    if !std::mem::replace(&mut seen[j], true) {
                        next.push(j);
                    } else if new {
                        next.push(i);
                    }
                }
            }
            next
        });
    // A body no visit reached was never held to its returns.
    for (v, seen) in values.iter_mut().zip(&seen) {
        if !seen {
            v.post.clear();
        }
    }
    let (closed, callees) = entered(&bodies, closed);
    let view = View {
        at: &at,
        values: &values,
        pairs: &pairs,
    };
    let pres = in_parallel(
        &bodies,
        weigh,
        || (),
        |(), (f, b)| match closed.contains(f) && any_check(&b.stmts) {
            true => entry_templates(b, decls, view),
            false => (BTreeSet::new(), None),
        },
    );
    let mut decided = Vec::new();
    for (i, (pre, d0)) in pres.into_iter().enumerate() {
        values[i].pre = pre;
        decided.extend(d0.map(|d| (i, d)));
    }
    // Every caller of a body with a `pre` proves it at each call row, or
    // the body loses the facts it does not. Lighter callers go first: a
    // `pre` they empty spares the heavier callers' walks. The intersection
    // does not depend on the order. Each batch takes two thirds of what is
    // left.
    let mut callers: Vec<&Body> = (bodies.iter().zip(&callees))
        .filter(|(_, gs)| gs.iter().any(|g| !values[at[g]].pre.is_empty()))
        .map(|((_, b), _)| *b)
        .collect();
    callers.sort_by_key(|b| b.names.len());
    while !callers.is_empty() {
        let view = View {
            at: &at,
            values: &values,
            pairs: &pairs,
        };
        callers.retain(|b| rows(&b.stmts).any(|(s, _)| view.pre(s).is_some()));
        let batch: Vec<&Body> = callers.drain(..(callers.len() * 2).div_ceil(3)).collect();
        let proved = in_parallel(
            &batch,
            |b| b.names.len(),
            || (),
            |(), b| entries(b, decls, view),
        );
        for (g, facts) in proved.into_iter().flatten() {
            values[g].pre.retain(|f| facts.contains(f));
        }
    }
    // A body decided without a `pre` is decided as an emitter would decide it
    // when it keeps none.
    let decided = (decided.into_iter())
        .filter(|(i, _)| values[*i].pre.is_empty())
        .map(|(i, d)| (bodies[i].0, Mutex::new(Some(d))))
        .collect();
    at.retain(|_, i| !values[*i].pre.is_empty() || !values[*i].post.is_empty());
    Summaries {
        at,
        values,
        decided,
        pairs,
    }
}

/// The [`Pairs`] every record value of the program keeps, by Houdini from
/// each pair of array fields of a declared record type, one of which a check
/// indexes.
///
/// Record types are compatible by shape, so a pair is keyed by its field
/// names and binds every record value with both fields, whatever its type. A
/// walk assumes a record name's pairs where the name takes a value it did
/// not build ([`Walk::restate`]). It tracks the lengths of a record that a
/// literal builds or a row resizes a field of ([`Walk::dirty`]), and that
/// record proves its pairs wherever its value leaves the name
/// ([`Walk::keeps`]). Each round walks the bodies that build or resize a
/// record with a pair and drops each pair a body does not prove; the next
/// round walks again each such body whose last walk assumed a dropped pair.
/// It stops when a round drops none; the live pair count bounds the rounds.
///
/// Before the rounds, a pair goes when a host-made type has both fields as
/// arrays or Strings, or when a row resizes a field of that name through a
/// place the walk does not track: a global, an element, a nested field, or
/// a name whose type is not a record.
fn invariants(
    bodies: &[(FnId, &Body)],
    scans: &[Scan<'_>],
    decls: &HashMap<String, TypeDecl>,
    records: &Records,
) -> Pairs {
    let host = &records.host;
    let resolve = |t: &Type| vyrn_frontend::types::resolve(t, decls);
    let seq = |t: &Type| {
        let t = resolve(t);
        t.is_seq() || t == Type::Str
    };
    let array = |t: &Type| matches!(resolve(t), Type::Array(_) | Type::SmallArray(..));
    let indexed: BTreeSet<&str> = scans
        .iter()
        .flat_map(|s| s.indexed.iter().copied())
        .collect();
    let lost: BTreeSet<&str> = scans.iter().flat_map(|s| s.lost.iter().copied()).collect();
    fn fields_of<'d>(d: &'d TypeDecl, keep: &dyn Fn(&Type) -> bool) -> Vec<&'d str> {
        match &d.base {
            Type::Record(fs) => (fs.iter())
                .filter(|f| keep(&f.ty))
                .map(|f| f.name.as_str())
                .collect(),
            _ => Vec::new(),
        }
    }
    let mut cands: BTreeSet<(&str, &str)> = BTreeSet::new();
    let mut gone: BTreeSet<(&str, &str)> = BTreeSet::new();
    for (name, d) in decls {
        let (fields, into) = match host.contains(name) {
            true => (fields_of(d, &seq), &mut gone),
            false => (fields_of(d, &array), &mut cands),
        };
        for (i, a) in fields.iter().enumerate() {
            for b in &fields[i + 1..] {
                into.insert(if a < b { (a, b) } else { (b, a) });
            }
        }
    }
    let wanted = |(a, b): &&(&str, &str)| {
        (indexed.contains(a) || indexed.contains(b)) && !lost.contains(a) && !lost.contains(b)
    };
    let mut of: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (a, b) in cands.difference(&gone).filter(wanted) {
        of.entry(a.to_string()).or_default().insert(b.to_string());
    }
    let mut pairs = Pairs::new(of, decls, &records.params);
    let maybe: Vec<&Body> = (bodies.iter().zip(scans))
        .filter(|(_, s)| s.builds)
        .map(|((_, b), _)| *b)
        .collect();
    let touches = in_parallel(
        &maybe,
        |b| b.names.len(),
        || (),
        |(), b| built(b, decls, &pairs).contains(&true),
    );
    let touching: Vec<&Body> = (maybe.iter().zip(touches))
        .filter_map(|(b, t)| t.then_some(*b))
        .collect();
    let mut used: Vec<BTreeSet<(String, String)>> = vec![BTreeSet::new(); touching.len()];
    let mut walk: Vec<usize> = (0..touching.len()).collect();
    while !pairs.is_empty() && !walk.is_empty() {
        let outs = in_parallel(
            &walk,
            |&i| touching[i].names.len(),
            || (),
            |(), &i| obliged(touching[i], decls, &pairs),
        );
        let mut dropped = BTreeSet::new();
        for (&i, (broken, u)) in walk.iter().zip(outs) {
            dropped.extend(broken);
            used[i] = u;
        }
        let mut of = std::mem::take(&mut pairs.of);
        for (a, b) in &dropped {
            if let Some(bs) = of.get_mut(a) {
                bs.remove(b);
            }
        }
        of.retain(|_, bs| !bs.is_empty());
        pairs = Pairs::new(of, decls, &records.params);
        walk = (0..touching.len())
            .filter(|&i| used[i].iter().any(|p| dropped.contains(p)))
            .collect();
    }
    pairs
}

/// What [`invariants`] reads of one body.
struct Scan<'b> {
    /// The fields a check indexes, directly or through a name read from one.
    indexed: Vec<&'b str>,
    /// The fields a row resizes through a place the walk does not track.
    lost: Vec<&'b str>,
    /// Whether a row builds a record or resizes a field.
    builds: bool,
}

fn scan<'b>(b: &'b Body, decls: &HashMap<String, TypeDecl>) -> Scan<'b> {
    let mut checked: BTreeSet<Name> = BTreeSet::new();
    let mut indexed: Vec<&str> = Vec::new();
    let mut lost: Vec<&str> = Vec::new();
    let mut reads: Vec<(Name, &str)> = Vec::new();
    let mut builds = false;
    for (s, _) in rows(&b.stmts) {
        match s {
            St::Check(c) => match &c.guard {
                Guard::Index(Place::Field(_, f), _) | Guard::Span(Place::Field(_, f), ..) => {
                    indexed.push(f)
                }
                Guard::Index(Place::Name(n), _) | Guard::Span(Place::Name(n), ..) => {
                    checked.insert(*n);
                }
                _ => {}
            },
            St::Let(n, Rhs::Read(Place::Field(_, f))) => reads.push((*n, f)),
            St::Let(_, Rhs::Make(Ctor::Record(..), _)) => builds = true,
            _ => {}
        }
        for p in resized_places(s) {
            let Place::Field(base, f) = p else { continue };
            builds = true;
            let tracked = match &**base {
                Place::Name(r) => {
                    let ty = &b.names[r.index()].ty;
                    vyrn_frontend::types::record_fields(ty, decls).is_some()
                }
                _ => false,
            };
            if !tracked {
                lost.push(f);
            }
        }
    }
    indexed.extend(
        reads
            .into_iter()
            .filter(|(n, _)| checked.contains(n))
            .map(|(_, f)| f),
    );
    Scan {
        indexed,
        lost,
        builds,
    }
}

/// Per name of `body`, whether it is a record a literal builds with both
/// fields of a pair, or a record whose field of a pair a row resizes.
fn built(body: &Body, decls: &HashMap<String, TypeDecl>, pairs: &Pairs) -> Vec<bool> {
    let mut out = vec![false; body.names.len()];
    for (s, _) in rows(&body.stmts) {
        match s {
            St::Let(n, Rhs::Make(Ctor::Record(_, fs), _)) => {
                out[n.index()] |= !pairs.within(fs).is_empty();
            }
            _ => {
                for p in resized_places(s) {
                    let Place::Field(r, f) = p else { continue };
                    let Place::Name(r) = &**r else { continue };
                    let ty = &body.names[r.index()].ty;
                    let paired = |(a, b): &(&str, &str)| a == f || b == f;
                    out[r.index()] |= pairs.of_type(ty, decls).iter().any(paired);
                }
            }
        }
    }
    out
}

/// The places row `s` may change the length of: a store's, a take's, and each
/// `modify` or `consume` argument's.
fn resized_places(s: &St) -> impl Iterator<Item = &Place> {
    let (one, args): (Option<&Place>, &[(Arg, Capability)]) = match s {
        St::Store { place, .. } => (Some(place), &[]),
        St::Let(_, Rhs::Take(p)) => (Some(p), &[]),
        St::Let(_, Rhs::Call { args, .. })
        | St::Do {
            rhs: Rhs::Call { args, .. },
            ..
        } => (None, args),
        _ => (None, &[]),
    };
    let args = args.iter().filter_map(|(a, cap)| match a {
        Arg::Place(p) if *cap != Capability::Read => Some(p),
        _ => None,
    });
    one.into_iter().chain(args)
}

/// The pairs of `pairs` that `body` does not prove of a record it builds or
/// resizes, where the record's value leaves the name ([`Walk::keeps`]), and
/// the pairs the walk assumed ([`Walk::restate`]).
fn obliged(
    body: &Body,
    decls: &HashMap<String, TypeDecl>,
    pairs: &Pairs,
) -> (BTreeSet<(String, String)>, BTreeSet<(String, String)>) {
    let at = HashMap::new();
    let view = View {
        at: &at,
        values: &[],
        pairs,
    };
    let mut stmts = body.stmts.clone();
    let mut w = Walk::new(body, decls, view, &stmts, Some(None));
    w.obliging = true;
    w.seed(&stmts, built(body, decls, pairs));
    let st = w.entry(&BTreeSet::new());
    let end = w.block(st, &mut stmts);
    w.exits(&end);
    (w.broken, w.assumed)
}

/// The bodies of `closed` that a direct call row of `bodies` enters, and no
/// row enters but a direct call passing every parameter in order: no row
/// names one by its spelling as a value, a `fn`-typed argument, a declared
/// release or another call's callee. A body no row calls is left out: it
/// would assume every candidate, and an entry the scan misses would be the
/// only one it has.
fn entered<'a>(
    bodies: &[(FnId, &'a Body)],
    closed: &HashSet<FnId>,
) -> (HashSet<FnId>, Vec<Vec<FnId>>) {
    let arity: HashMap<FnId, usize> = (bodies.iter())
        .filter(|(f, _)| closed.contains(f))
        .map(|(f, b)| (*f, b.params.len()))
        .collect();
    let names: HashSet<&str> = (bodies.iter())
        .filter(|(f, _)| closed.contains(f))
        .map(|(_, b)| &*b.name)
        .collect();
    // Per body: the names of `closed` it spells, and the bodies of `closed`
    // its direct calls enter, each with whether the call passes every
    // parameter in order.
    let scans = in_parallel(
        bodies,
        |(_, b)| b.names.len(),
        || (),
        |(), (_, b)| {
            let mut spelled: Vec<&str> = Vec::new();
            let mut calls: Vec<(FnId, bool)> = Vec::new();
            let mut spell = |n: &'a str| {
                if names.contains(n) {
                    spelled.push(n);
                }
            };
            b.names.iter().flat_map(|i| &i.runs).for_each(|n| spell(n));
            for (s, _) in rows(&b.stmts) {
                let (St::Let(_, rhs) | St::Do { rhs, .. }) = s else {
                    continue;
                };
                match rhs {
                    Rhs::Call {
                        callee,
                        args,
                        kind,
                        solved,
                        targets,
                        ..
                    } => {
                        targets.iter().filter_map(spelling).for_each(&mut spell);
                        match kind {
                            Callee::Fn(g) if arity.contains_key(g) => {
                                let fits = arity[g] == args.len();
                                calls.push((*g, fits && solved.is_empty() && targets.is_empty()));
                            }
                            Callee::Fn(_) => {}
                            // A builtin enters a declared function only
                            // through its route, a runtime module's `$`
                            // spelling.
                            Callee::Builtin | Callee::Ctor => {}
                            _ => spell(callee),
                        }
                    }
                    Rhs::Make(Ctor::Closure(t), _) => spelling(t).into_iter().for_each(&mut spell),
                    Rhs::Prim(Op::Closure(key), ..) => spell(key),
                    _ => {}
                }
            }
            (spelled, calls)
        },
    );
    let mut spelled: HashSet<&str> = HashSet::new();
    let mut called: HashSet<FnId> = HashSet::new();
    let mut odd: HashSet<FnId> = HashSet::new();
    let mut callees = Vec::with_capacity(bodies.len());
    for (s, calls) in scans {
        spelled.extend(s);
        for (g, fits) in &calls {
            called.insert(*g);
            if !fits {
                odd.insert(*g);
            }
        }
        callees.push(calls.into_iter().map(|(g, _)| g).collect());
    }
    let entered = (bodies.iter())
        .filter(|(f, _)| called.contains(f) && !odd.contains(f))
        .filter(|(_, b)| !spelled.contains(&*b.name))
        .map(|(f, _)| *f)
        .collect();
    (entered, callees)
}

/// The function or lambda a target names.
fn spelling(t: &Target) -> Option<&str> {
    match t {
        Target::Fn(n) | Target::Value(n) | Target::Lambda(n, ..) => Some(n),
        Target::Param(_) => None,
    }
}

/// The summarized bodies whose result a check of `b` can depend on: each
/// callee of a direct call relevant to a check ([`relevant`]).
fn seeds(b: &Body, decls: &HashMap<String, TypeDecl>, sums: View<'_>) -> Vec<usize> {
    let rel = relevant(
        b,
        decls,
        &b.stmts,
        false,
        true,
        &|g| sums.post(g),
        &|_, _| false,
    );
    (rows(&b.stmts))
        .filter_map(|(s, _)| match s {
            St::Let(n, rhs) if rel[n.index()] => {
                let (i, sum) = sums.get(direct_call(rhs)?)?;
                (!sum.post.is_empty()).then_some(i)
            }
            _ => None,
        })
        .collect()
}

/// The callee of a direct call whose result a summary may state: no type
/// arguments, so the row names the body that runs, and no `fn`-typed
/// argument, so the arguments are the parameters in order.
fn direct_call(rhs: &Rhs) -> Option<FnId> {
    match rhs {
        Rhs::Call {
            kind: Callee::Fn(g),
            solved,
            targets,
            ..
        } if solved.is_empty() && targets.is_empty() => Some(*g),
        _ => None,
    }
}

/// `body`'s summary with every candidate of its `post` and no `pre`. The
/// first return of a name or an integer decides the result: an integer's
/// value or an array's length; any other result, a String's included,
/// states nothing. The parameters are those the body never writes. An
/// `UInt64` is left out: the facts read its values above `i64::MAX` as
/// negatives.
fn templates(body: &Body, decls: &HashMap<String, TypeDecl>) -> Summary {
    let params: Vec<Kind> = (body.params.iter())
        .map(|p| kind_of(&body.names[p.index()].ty, decls))
        .collect();
    let r = rows(&body.stmts).find_map(|(s, _)| match s {
        St::Return {
            value: Some(Val::Name(n)),
            ..
        } => {
            let ty = &body.names[n.index()].ty;
            Some(match kind_of(ty, decls) {
                k if k.is_int() => Some(Term::Val(Name(0))),
                _ if vyrn_frontend::types::resolved(ty, decls).is_seq() => Some(Term::Len(Name(0))),
                _ => None,
            })
        }
        St::Return {
            value: Some(Val::Lit(Lit::Int(_) | Lit::Byte(_))),
            ..
        } => Some(Some(Term::Val(Name(0)))),
        _ => None,
    });
    let mut post = BTreeSet::new();
    if let Some(Some(r)) = r {
        let mut written = BTreeSet::new();
        writes(&body.stmts, &mut written);
        let usable = |k: usize| !written.contains(&body.params[k]);
        let ints = interface(&params, |k, kind| usable(k) && kind.is_int(), Term::Val);
        let seqs = interface(&params, |k, kind| usable(k) && kind == Kind::Seq, Term::Len);
        if let Term::Val(_) = r {
            let r = Lin::of(r);
            post.extend(
                [Some(r.clone()), r.plus(-1), r.plus(1)]
                    .into_iter()
                    .flatten(),
            );
            for a in &seqs {
                post.extend(a.sub(&r));
                post.extend(a.sub(&r).and_then(|l| l.plus(-1)));
                for p in &ints {
                    post.extend(a.sub(p).and_then(|l| l.sub(&r)));
                }
            }
            for p in &ints {
                post.extend(r.sub(p));
                post.extend(p.sub(&r));
            }
        } else {
            let r = Lin::of(r);
            post.extend(seqs.iter().filter_map(|a| r.sub(a)));
        }
    }
    Summary {
        params,
        pre: BTreeSet::new(),
        post,
    }
}

/// Parameter `k`'s term `term(Name(k + 1))`, for each `k` whose kind `keep`
/// takes.
fn interface(
    params: &[Kind],
    keep: impl Fn(usize, Kind) -> bool,
    term: fn(Name) -> Term,
) -> Vec<Lin> {
    (0..params.len())
        .filter(|&k| keep(k, params[k]))
        .map(|k| Lin::of(term(Name(k as u32 + 1))))
        .collect()
}

/// The candidates of the `pre` of `body`, a body only call rows enter, and
/// `body` decided as [`decide`] decides it with no `pre`, when that walk
/// ran. The candidates are `p >= 0`, `p >= 1`, `q - p >= 0`,
/// `q - p - 1 >= 0`, `len(a) >= 1`, `len(a) - p >= 0` and
/// `len(a) - p - 1 >= 0` over the integer and array or String parameters
/// ([`interface`]) a check that walk keeps can depend on ([`relevant`]):
/// each candidate makes every caller walk. An `UInt64` is left out, as in
/// [`templates`].
fn entry_templates(
    body: &Body,
    decls: &HashMap<String, TypeDecl>,
    sums: View<'_>,
) -> (BTreeSet<Lin>, Option<Body>) {
    let params: Vec<Kind> = (body.params.iter())
        .map(|p| kind_of(&body.names[p.index()].ty, decls))
        .collect();
    let candidates = |ss: &[St]| {
        let rel = relevant(body, decls, ss, false, true, &|g| sums.post(g), &|_, _| {
            false
        });
        let used = |k: usize| rel[body.params[k].index()];
        let ints = interface(&params, |k, kind| used(k) && kind.is_int(), Term::Val);
        let seqs = interface(&params, |k, kind| used(k) && kind == Kind::Seq, Term::Len);
        let mut out = BTreeSet::new();
        for p in &ints {
            out.insert(p.clone());
            out.extend(p.plus(-1));
            for q in ints.iter().filter(|q| *q != p) {
                out.extend(q.sub(p));
                out.extend(q.sub(p).and_then(|l| l.plus(-1)));
            }
        }
        for a in &seqs {
            out.extend(a.plus(-1));
            for p in &ints {
                out.extend(a.sub(p));
                out.extend(a.sub(p).and_then(|l| l.plus(-1)));
            }
        }
        out
    };
    if candidates(&body.stmts).is_empty() {
        return (BTreeSet::new(), None);
    }
    let mut d0 = body.clone();
    decide_in(&mut d0, decls, sums);
    (candidates(&d0.stmts), Some(d0))
}

/// The facts of `cands`' `post` every live return of `body` proves, walking
/// it with the callees' facts `sums` states, and the summaries it took facts
/// from, by index. A path that ends without a value proves none.
fn returns(
    body: &Body,
    decls: &HashMap<String, TypeDecl>,
    sums: View<'_>,
    cands: &Summary,
) -> (BTreeSet<Lin>, BTreeSet<usize>) {
    let mut stmts = body.stmts.clone();
    let mut w = Walk::new(body, decls, sums, &stmts, Some(Some(cands.post.clone())));
    let st = w.entry(&BTreeSet::new());
    let end = w.block(st, &mut stmts);
    match w.post {
        Some(kept) if end.dead => (kept, w.read),
        _ => (BTreeSet::new(), w.read),
    }
}

/// Per callee of `body` with a `pre`, by index: the facts of it every live
/// call row of `body` proves, walking it with the callees' facts `sums`
/// states and none at its own entry.
fn entries(
    body: &Body,
    decls: &HashMap<String, TypeDecl>,
    sums: View<'_>,
) -> BTreeMap<usize, BTreeSet<Lin>> {
    let mut stmts = body.stmts.clone();
    let mut w = Walk::new(body, decls, sums, &stmts, Some(None));
    let st = w.entry(&BTreeSet::new());
    w.block(st, &mut stmts);
    w.entered
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
    sums: View<'a>,
    /// Whether the walk serves the solve ([`summaries`]): it writes no
    /// verdict, and gathers `post`, `entered` and `read`.
    solving: bool,
    /// While solving: the facts of the body's `post` every return met so
    /// far proves; `None` when it has none.
    post: Option<BTreeSet<Lin>>,
    /// While solving: per callee with a `pre`, by index, the facts of it
    /// every call row met so far proves.
    entered: BTreeMap<usize, BTreeSet<Lin>>,
    /// While solving: the summaries a call took `post` facts from.
    read: BTreeSet<usize>,
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
    /// Whether the walk serves [`invariants`]: it gathers `broken`.
    obliging: bool,
    /// The record names whose field lengths the walk tracks from a literal or
    /// a row that resized a field, rather than assuming their [`Pairs`].
    dirty: BTreeSet<Name>,
    /// While obliging: the pairs a record of `dirty` did not prove where its
    /// value left the name.
    broken: BTreeSet<(String, String)>,
    /// While obliging: the pairs [`Walk::restate`] assumed.
    assumed: BTreeSet<(String, String)>,
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
    Cond(Vec<Fact>, Vec<Fact>),
    Facts(Vec<Lin>),
    Nothing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// An integer of this many bits, signed or not.
    Int(u8, bool),
    /// An array or a String: it has a length.
    Seq,
    Other,
}

impl Kind {
    /// An integer whose every value is a linear term: not an `UInt64`.
    fn is_int(self) -> bool {
        matches!(self, Kind::Int(bits, signed) if bits < 64 || signed)
    }
}

fn kind_of(ty: &Type, decls: &HashMap<String, TypeDecl>) -> Kind {
    match &*vyrn_frontend::types::resolved(ty, decls) {
        t if t.is_seq() || *t == Type::Str => Kind::Seq,
        t => match vyrn_frontend::validate::width(t) {
            Some((bits, signed)) => Kind::Int(bits, signed),
            None => Kind::Other,
        },
    }
}

impl<'a> Walk<'a> {
    /// A walk that decides `body`'s checks when `solve` is `None`, and
    /// otherwise serves the solve with the `post` candidates it holds.
    fn new(
        body: &'a Body,
        decls: &'a HashMap<String, TypeDecl>,
        sums: impl Into<View<'a>>,
        stmts: &[St],
        solve: Option<Option<BTreeSet<Lin>>>,
    ) -> Walk<'a> {
        let sums = sums.into();
        let solving = solve.is_some();
        let post = solve.flatten();
        // Argument `k` of a call to a body with a `pre` that names parameter `k`.
        let enters = |g: FnId, k: usize| {
            let named = |f: &Lin| f.terms.iter().any(|(t, _)| t.name().index() == k + 1);
            solving && sums.get(g).is_some_and(|(_, s)| s.pre.iter().any(named))
        };
        let linked = |g: FnId| sums.post(g);
        Walk {
            body,
            decls,
            sums,
            slot: relevant(
                body,
                decls,
                stmts,
                post.is_some(),
                !solving,
                &linked,
                &enters,
            )
            .into_iter()
            .map(|relevant| Slot {
                relevant,
                origin: Origin::Local,
            })
            .collect(),
            solving,
            post,
            entered: BTreeMap::new(),
            read: BTreeSet::new(),
            loops: Vec::new(),
            record: true,
            memo: HashMap::new(),
            open: BTreeSet::new(),
            refuted: Vec::new(),
            obliging: false,
            dirty: BTreeSet::new(),
            broken: BTreeSet::new(),
            assumed: BTreeSet::new(),
        }
    }

    /// The state at the body's entry: each parameter a fresh value, with
    /// the facts `pre` states over the interface ([`Summary`]) and the
    /// parameters' clauses.
    fn entry(&mut self, pre: &BTreeSet<Lin>) -> State {
        let mut st = State::default();
        let params = &self.body.params;
        for p in params {
            self.slot[p.index()].origin = Origin::Param(*p);
            self.fresh(&mut st, *p);
        }
        let at = |t: Term| {
            let p = *params.get(t.name().index().checked_sub(1)?)?;
            match t {
                Term::Val(_) => Some(Lin::of(Term::Val(p))),
                Term::Len(_) => Some(Lin::of(Term::Len(p))),
                Term::Col(..) | Term::Field(..) => None,
            }
        };
        for f in pre {
            if let Some(l) = f.map(at) {
                st.assume(&l);
            }
        }
        for a in &self.body.assumes {
            self.holds(&mut st, a);
        }
        st
    }

    /// Assumes the facts the comparison `a` states, those over terms.
    fn holds(&self, st: &mut State, a: &Atom) {
        for f in self.atom(a).into_iter().flatten() {
            match f {
                Fact::Ge(l) => st.assume(&l),
                Fact::Ne(l) => st.differ(&l),
            }
        }
    }

    /// What the comparison `a` states over the walk's terms; `None` when a
    /// side is no term, or its type is an `UInt64`, whose values above
    /// `i64::MAX` a term reads as negatives.
    fn atom(&self, a: &Atom) -> Option<Vec<Fact>> {
        if !kind_of(&a.ty, self.decls).is_int() {
            return None;
        }
        let d = self.operand(&a.l)?.sub(&self.operand(&a.r)?)?;
        Some(match a.op.compare()? {
            Cmp::Order { strict, flipped } => {
                let d = if flipped { d.scale(-1)? } else { d };
                vec![Fact::Ge(if strict { d.plus(-1)? } else { d })]
            }
            Cmp::Equal { negated: false } => vec![Fact::Ge(d.scale(-1)?), Fact::Ge(d)],
            Cmp::Equal { negated: true } => vec![Fact::Ne(d)],
        })
    }

    /// An operand of a clause as a term: a value, a length, or an integer
    /// field of a `read` parameter ([`Walk::field`]).
    fn operand(&self, o: &Operand) -> Option<Lin> {
        match (&o.of, o.part.as_deref()) {
            (v, None) => self.lin(v),
            (Val::Lit(Lit::Str(s)), Some("byteLength")) => Some(Lin::k(s.len() as i64)),
            (Val::Name(n), Some("length" | "byteLength")) => {
                self.length(&Place::Name(*n)).map(Lin::of)
            }
            (Val::Name(n), Some(f)) => self.field(&Place::Name(*n), f).map(Lin::of),
            _ => None,
        }
    }

    fn kind(&self, n: Name) -> Kind {
        kind_of(&self.body.names[n.index()].ty, self.decls)
    }

    /// The length of `p`: an array or String name, or such a field of a record
    /// name ([`Walk::col`]).
    fn length(&self, p: &Place) -> Option<Term> {
        match p {
            Place::Name(b) if matches!(self.kind(*b), Kind::Seq) => Some(Term::Len(*b)),
            Place::Field(r, f) => {
                let Place::Name(r) = &**r else { return None };
                self.col_term(*r, f)
            }
            _ => None,
        }
    }

    /// The length of the array or String field `f` of record name `r`.
    fn col_term(&self, r: Name, f: &str) -> Option<Term> {
        let (own, least) = self.col(r, f)?;
        let at = if self.open.contains(&r) { own } else { least };
        Some(Term::Col(r, at))
    }

    /// The [`Pairs`] record name `n`'s type has, each as its two fields'
    /// length terms and its field names.
    fn pairs_of(&self, n: Name) -> Vec<(Term, Term, &'a str, &'a str)> {
        let pairs = self.sums.pairs;
        if pairs.is_empty() || self.kind(n) != Kind::Other {
            return Vec::new();
        }
        let ty = &self.body.names[n.index()].ty;
        (pairs.of_type(ty, self.decls).into_iter())
            .filter_map(|(a, b)| Some((self.col_term(n, a)?, self.col_term(n, b)?, a, b)))
            .collect()
    }

    /// Assumes the [`Pairs`] of record name `n`, whose value a walk of another
    /// row or body proved them of ([`invariants`]).
    fn restate(&mut self, st: &mut State, n: Name) {
        for (a, b, fa, fb) in self.pairs_of(n) {
            st.assume_eq(&Lin::of(a), &Lin::of(b));
            if self.obliging {
                self.assumed.insert((fa.to_string(), fb.to_string()));
            }
        }
    }

    /// While obliging, drops each pair record name `n` of [`Walk::dirty`] does
    /// not prove in `st`, where its value leaves the name for a reader that
    /// assumes the pairs: a call, a store, a literal, a return or a release.
    fn keeps(&mut self, st: &State, n: Name) {
        if !self.obliging || !self.record || st.dead || !self.dirty.contains(&n) {
            return;
        }
        let holds = |g: &Lin| st.ge0(g).is_some_and(|cert| cert.verify(st, g));
        for (a, b, fa, fb) in self.pairs_of(n) {
            let d = Lin::of(a).sub(&Lin::of(b));
            let both = |d: &Lin| holds(d) && d.scale(-1).is_some_and(|e| holds(&e));
            if !d.as_ref().is_some_and(both) {
                self.broken.insert((fa.to_string(), fb.to_string()));
            }
        }
    }

    /// [`Walk::keeps`] of each name `rhs` reads whole: not through a field
    /// or an element.
    fn observe(&mut self, st: &State, rhs: &Rhs) {
        if !self.obliging {
            return;
        }
        let val = |v: &Val| match v {
            Val::Name(n) => Some(*n),
            Val::Lit(_) => None,
        };
        let names: Vec<Name> = match rhs {
            Rhs::Val(v) => val(v).into_iter().collect(),
            Rhs::Read(Place::Name(n)) | Rhs::Take(Place::Name(n)) => vec![*n],
            Rhs::Call { args, .. } => (args.iter())
                .filter_map(|(a, _)| match a {
                    Arg::Val(v) => val(v),
                    Arg::Place(Place::Name(n)) => Some(*n),
                    Arg::Place(_) => None,
                })
                .collect(),
            Rhs::Prim(_, vs, _) | Rhs::Make(_, vs) => vs.iter().filter_map(val).collect(),
            Rhs::Read(_) | Rhs::Take(_) => Vec::new(),
        };
        for n in names {
            self.keeps(st, n);
        }
    }

    /// [`Walk::keeps`] of each `modify` parameter, at an exit: the caller
    /// assumes its pairs after the call.
    fn exits(&mut self, st: &State) {
        if !self.obliging {
            return;
        }
        for k in 0..self.body.params.len() {
            let p = self.body.params[k];
            let modify = matches!(
                &self.body.names[p.index()].borrow_kind,
                Some(BorrowKind::Param { cap: "modify", .. })
            );
            if modify {
                self.keeps(st, p);
            }
        }
    }

    /// Notes that a row resized field `f` of record name `r`, when a pair of
    /// `r`'s type names `f`. A borrow that is no parameter proves its pairs
    /// after the row: its source is read through another name, which assumes
    /// them.
    fn resized_field(&mut self, st: &State, r: Name, f: &str) {
        if !self.pairs_of(r).iter().any(|(.., a, b)| *a == f || *b == f) {
            return;
        }
        self.dirty.insert(r);
        let info = &self.body.names[r.index()];
        if info.borrow_kind.is_some() && !self.body.params.contains(&r) {
            self.keeps(st, r);
        }
    }

    /// Makes each name `rel` marks relevant, and every name linked to one.
    fn seed(&mut self, ss: &[St], mut rel: Vec<bool>) {
        for (r, s) in rel.iter_mut().zip(&self.slot) {
            *r |= s.relevant;
        }
        let sums = self.sums;
        loop {
            let before = rel.iter().filter(|r| **r).count();
            mark(self.body, self.decls, ss, &mut rel, false, &|g| {
                sums.post(g)
            });
            if rel.iter().filter(|r| **r).count() == before {
                break;
            }
        }
        for (s, r) in self.slot.iter_mut().zip(rel) {
            s.relevant = r;
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

    /// The integer field `f` of `r` as one term, when `r` is a `read`
    /// parameter: by the module's postulate its value changes only where the
    /// walk sees `r` written, and then [`State::kill`] forgets it.
    fn field(&self, r: &Place, f: &str) -> Option<Term> {
        let Place::Name(r) = r else { return None };
        let info = &self.body.names[r.index()];
        let read = matches!(
            &info.borrow_kind,
            Some(BorrowKind::Param { cap: "read", .. })
        );
        if !read || !self.body.params.contains(r) {
            return None;
        }
        let fields = vyrn_frontend::types::record_fields(&info.ty, self.decls)?;
        let at = fields.iter().position(|x| x.name == f)?;
        Some(Term::Field(*r, u32::try_from(at).ok()?))
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
    fn fresh(&mut self, st: &mut State, n: Name) {
        st.kill(n);
        self.range(st, n);
    }

    /// What `n`'s type says about it, unless a definition says more.
    fn range(&mut self, st: &mut State, n: Name) {
        if self.slot[n.index()].relevant
            && !st.defs.contains_key(&Term::Val(n))
            && !st.defs.contains_key(&Term::Len(n))
        {
            self.span(st, n);
        }
    }

    /// What `n`'s type says about its value or its length.
    fn span(&mut self, st: &mut State, n: Name) {
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
            Kind::Other if !self.dirty.contains(&n) => self.restate(st, n),
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

    /// `r`, when the state proves it stays in `-EXACT..=EXACT`: as a whole,
    /// or because `r` is `a + b` or `a - b` over `parts` `[a, b]`, each in
    /// half that range. The search chains two premises, so one bound per
    /// operand reaches a sum whose bound needs three.
    fn fits(st: &State, r: Option<Lin>, parts: &[&Lin]) -> Option<Lin> {
        let within = |l: &Lin, m: i64| {
            let (Some(hi), Some(lo)) = (Lin::k(m).sub(l), l.plus(m)) else {
                return false;
            };
            st.ge0(&hi).is_some() && st.ge0(&lo).is_some()
        };
        let r = r?;
        let halves = || parts.len() == 2 && parts.iter().all(|p| within(p, EXACT / 2));
        (within(&r, EXACT) || halves()).then_some(r)
    }

    fn block(&mut self, mut st: State, ss: &mut [St]) -> State {
        for s in ss {
            // A summary every return so far refuted gains nothing from the rest.
            if self.post.as_ref().is_some_and(BTreeSet::is_empty) {
                return State::dead();
            }
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
                self.observe(&st, rhs);
                self.enter(&st, rhs);
                self.bind(&mut st, *n, rhs);
                st
            }
            St::Do { rhs, .. } => {
                self.observe(&st, rhs);
                self.enter(&st, rhs);
                self.effects(&mut st, rhs);
                // The rule holds again after its check.
                if let Some(r) = rhs.checks_rule(&self.body.names) {
                    self.open.remove(&r);
                }
                st
            }
            St::Store { place, value, .. } => {
                if let Val::Name(m) = value {
                    self.keeps(&st, *m);
                }
                if let Place::Name(n) = place {
                    // The value the store displaces is released.
                    self.keeps(&st, *n);
                    let o = self.slot[n.index()].origin.join(self.origin_of_val(value));
                    self.slot[n.index()].origin = o;
                }
                self.open(&mut st, place);
                match (&*place, self.length(place)) {
                    // The field takes the stored array's length.
                    (Place::Field(r, f), Some(t)) => {
                        st.forget(t);
                        let stored = match value {
                            Val::Name(v) => self.length(&Place::Name(*v)),
                            Val::Lit(_) => None,
                        };
                        if let Some(l) = stored {
                            st.define(t, &Lin::of(l));
                        }
                        if let Place::Name(r) = &**r {
                            self.resized_field(&st, *r, f);
                        }
                    }
                    // A store into a field that is no array or String keeps
                    // every length.
                    (Place::Field(..), None) => {}
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
                self.keeps(&st, *n);
                st.kill(*n);
                st
            }
            St::Row { name, .. } => {
                self.keeps(&st, *name);
                st
            }
            St::If {
                cond, then, els, ..
            } => {
                let (yes, no) = match cond {
                    Val::Lit(Lit::Bool(true)) => (st.clone(), State::dead()),
                    Val::Lit(Lit::Bool(false)) => (State::dead(), st.clone()),
                    Val::Name(c) => {
                        let (t, f) = st.conds.get(c).cloned().unwrap_or_default();
                        let (mut yes, mut no) = (st.clone(), st.clone());
                        t.iter().for_each(|l| yes.add(l));
                        f.iter().for_each(|l| no.add(l));
                        (yes, no)
                    }
                    _ => (st.clone(), st.clone()),
                };
                let mut a = self.block(yes, then);
                let mut b = self.block(no, els);
                if !a.dead && !b.dead {
                    leave(&mut a, then, &[]);
                    leave(&mut b, els, &[]);
                }
                State::join(&[a, b])
            }
            St::Block { body, .. } => self.block(st, body),
            St::Loop { body, .. } => self.looped(st, body),
            St::Switch { on, arms, .. } => {
                let mut outs = vec![st.clone()];
                for a in arms.iter_mut() {
                    let mut s = st.clone();
                    for b in &a.binds {
                        self.slot[b.index()].origin = self.origin_of_val(on);
                        self.fresh(&mut s, *b);
                    }
                    outs.push(self.block(s, &mut a.body));
                }
                if outs.iter().filter(|s| !s.dead).count() > 1 {
                    for (s, a) in outs[1..].iter_mut().zip(arms.iter()) {
                        leave(s, &a.body, &a.binds);
                    }
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
            St::Return { value, .. } => {
                if self.record {
                    self.returned(&st, value.as_ref());
                }
                if let Some(Val::Name(m)) = value {
                    self.keeps(&st, *m);
                }
                self.exits(&st);
                State::dead()
            }
            St::Trap => State::dead(),
            St::Check(c) => {
                let goals = self.goals(&st, &c.guard);
                // A solve's walk writes no verdict: its copy of the rows is dropped.
                if self.record && !self.solving {
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
                if let Guard::NonZero(d) = &c.guard {
                    if let Some(d) = self.lin(d) {
                        st.differ(&d);
                    }
                }
                if let Guard::Clause { atoms, .. } = &c.guard {
                    atoms.iter().for_each(|a| self.holds(&mut st, a));
                }
                st
            }
        }
    }

    /// What must be `>= 0` for the check to pass, as far as linear facts can
    /// say; `None` when they cannot say it all.
    fn goals(&self, st: &State, g: &Guard) -> Option<Vec<Lin>> {
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
                    false if st.differs(&d) => vec![],
                    // Otherwise only a divisor of one sign proves nonzero.
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
            // A disequality is shown by the state, not by a goal.
            Guard::Clause { atoms, .. } => {
                let mut out = Vec::new();
                for f in atoms
                    .iter()
                    .map(|a| self.atom(a))
                    .collect::<Option<Vec<_>>>()?
                {
                    for f in f {
                        match f {
                            Fact::Ge(l) => out.push(l),
                            Fact::Ne(l) if st.differs(&l) => {}
                            Fact::Ne(_) => return None,
                        }
                    }
                }
                out
            }
        })
    }

    fn bind(&mut self, st: &mut State, n: Name, rhs: &Rhs) {
        let o = self.origin_of(n, rhs);
        self.slot[n.index()].origin = self.slot[n.index()].origin.join(o);
        st.kill(n);
        // A literal's lengths are its parts', not the pairs'.
        if let Rhs::Make(Ctor::Record(..), _) = rhs {
            self.dirty.insert(n);
        }
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
            Rhs::Read(Place::Field(r, f)) if matches!(self.kind(n), Kind::Int(..)) => {
                if let Some(t) = self.field(r, f) {
                    st.define(Term::Val(n), &Lin::of(t));
                    self.span(st, n);
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
            Rhs::Make(Ctor::Record(_, fields), parts) if self.rule(n).is_none() => {
                for (f, v) in fields.iter().zip(parts) {
                    let len = match v {
                        Val::Name(m) => self.length(&Place::Name(*m)).map(Lin::of),
                        Val::Lit(Lit::Str(x)) => Some(Lin::k(x.len() as i64)),
                        Val::Lit(_) => None,
                    };
                    if let (Some(t), Some(len)) = (self.col_term(n, f), len) {
                        st.define(t, &len);
                    }
                }
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
        self.called(st, n, rhs);
    }

    /// Assumes what the callee's summary states of `n = rhs`, a direct call,
    /// read after the call's effects. A fact naming an argument the call may
    /// write is left out, and so is one with a term no argument gives.
    fn called(&mut self, st: &mut State, n: Name, rhs: &Rhs) {
        let Some((i, s)) = direct_call(rhs).and_then(|g| self.sums.get(g)) else {
            return;
        };
        let Rhs::Call { args, .. } = rhs else { return };
        if s.post.is_empty() || args.len() != s.params.len() {
            return;
        }
        if self.solving {
            self.read.insert(i);
        }
        // A scalar argument is a copy, whatever its capability.
        let copied = |k: usize| args[k].1 == Capability::Read || s.params[k].is_int();
        let written: Vec<Name> = (0..args.len())
            .filter(|&k| !copied(k))
            .filter_map(|k| root(&args[k].0))
            .collect();
        let at = |t: Term| -> Option<Lin> {
            let Some(k) = t.name().index().checked_sub(1) else {
                return match t {
                    Term::Val(_) if self.kind(n).is_int() => Some(Lin::of(Term::Val(n))),
                    Term::Len(_) if self.kind(n) == Kind::Seq => Some(Lin::of(Term::Len(n))),
                    _ => None,
                };
            };
            self.arg(t, &args.get(k).filter(|_| copied(k))?.0, s.params[k])
        };
        for f in &s.post {
            let free = |l: &Lin| !l.terms.iter().any(|(t, _)| written.contains(&t.name()));
            if let Some(l) = f.map(at).filter(free) {
                st.assume(&l);
            }
        }
    }

    /// Keeps, of the `pre` of the callee of `rhs`, a direct call, the facts
    /// the state proves of the arguments as passed: before the call writes
    /// any. Only the solve's recording walk keeps them; Houdini's rounds
    /// meet the row again in the replay from the settled head.
    fn enter(&mut self, st: &State, rhs: &Rhs) {
        if !self.solving || !self.record {
            return;
        }
        let Some((i, s)) = direct_call(rhs).and_then(|g| self.sums.get(g)) else {
            return;
        };
        let Rhs::Call { args, .. } = rhs else { return };
        if s.pre.is_empty() {
            return;
        }
        let at = |t: Term| {
            let k = t.name().index().checked_sub(1)?;
            self.arg(t, &args.get(k)?.0, *s.params.get(k)?)
        };
        let holds = |g: &Lin| st.ge0(g).is_some_and(|cert| cert.verify(st, g));
        let kept = (self.entered.get(&i).unwrap_or(&s.pre).iter())
            .filter(|f| f.map(at).is_some_and(|g| holds(&g)))
            .cloned()
            .collect();
        self.entered.insert(i, kept);
    }

    /// Parameter `k`'s term `t`, of kind `kind`, as the call's argument `a`
    /// gives it in the caller.
    fn arg(&self, t: Term, a: &Arg, kind: Kind) -> Option<Lin> {
        match (t, a) {
            (Term::Val(_), Arg::Val(Val::Lit(_))) => self.arg_lin(a),
            (Term::Val(_), _) if self.arg_kind(a)? == kind => self.arg_lin(a),
            (Term::Len(_), Arg::Val(Val::Lit(Lit::Str(x)))) => Some(Lin::k(x.len() as i64)),
            (Term::Len(_), Arg::Val(Val::Name(m))) => Some(Lin::of(self.length(&Place::Name(*m))?)),
            (Term::Len(_), Arg::Place(p)) => Some(Lin::of(self.length(p)?)),
            _ => None,
        }
    }

    /// The linear value of a call argument that is an integer.
    fn arg_lin(&self, a: &Arg) -> Option<Lin> {
        match a {
            Arg::Val(v) => self.lin(v),
            Arg::Place(Place::Name(m)) => self.lin(&Val::Name(*m)),
            Arg::Place(_) => None,
        }
    }

    fn arg_kind(&self, a: &Arg) -> Option<Kind> {
        match a {
            Arg::Val(Val::Name(m)) | Arg::Place(Place::Name(m)) => Some(self.kind(*m)),
            _ => None,
        }
    }

    /// Keeps the summary candidates the state proves of the returned `value`.
    fn returned(&mut self, st: &State, value: Option<&Val>) {
        let Some(cands) = self.post.take() else {
            return;
        };
        let params = &self.body.params;
        let at = |t: Term| -> Option<Lin> {
            match (t.name().index().checked_sub(1), t, value?) {
                (None, Term::Val(_), v) => self.lin(v),
                (None, Term::Len(_), Val::Name(m)) => Some(Lin::of(self.length(&Place::Name(*m))?)),
                (Some(k), Term::Val(_), _) => Some(Lin::of(Term::Val(*params.get(k)?))),
                (Some(k), Term::Len(_), _) => Some(Lin::of(Term::Len(*params.get(k)?))),
                _ => None,
            }
        };
        let holds = |g: &Lin| st.ge0(g).is_some_and(|cert| cert.verify(st, g));
        let kept = (cands.into_iter())
            .filter(|c| c.map(at).is_some_and(|g| holds(&g)))
            .collect();
        self.post = Some(kept);
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
                // A copy of a name of the same width and sign holds its value,
                // which the type already bounds.
                let copy = matches!(v, Val::Name(m) if self.kind(*m) == self.kind(n));
                if let Some(l) = self.lin(v).and_then(|l| st.norm(&l)) {
                    if copy || self.in_range(n, &l, st) {
                        st.define(Term::Val(n), &l);
                    }
                }
            }
            // A Bool copy carries the facts its source's truth gives, none
            // when it has none; a literal's other side is dead (`-1 >= 0`).
            (Val::Name(m), _) if self.body.names[n.index()].ty == Type::Bool => {
                let c = st.conds.get(m).cloned().unwrap_or_default();
                st.conds.insert(n, c);
            }
            (Val::Lit(Lit::Bool(b)), _) => {
                let never = vec![Fact::Ge(Lin::k(-1))];
                let c = if *b {
                    (Vec::new(), never)
                } else {
                    (never, Vec::new())
                };
                st.conds.insert(n, c);
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
        let exact = |r: Option<Lin>, parts: &[&Lin]| {
            if self.is_int64(n) {
                Self::fits(pre, r, parts)
            } else {
                None
            }
        };
        match (op, vs) {
            (Op::Bin(o), [a, b]) => {
                let (la, lb) = (self.lin(a), self.lin(b));
                let parts: Vec<&Lin> = la.iter().chain(&lb).collect();
                let r = match o {
                    BinOp::Add => exact(
                        la.as_ref().zip(lb.as_ref()).and_then(|(a, b)| a.add(b)),
                        &parts,
                    ),
                    BinOp::Sub => exact(
                        la.as_ref().zip(lb.as_ref()).and_then(|(a, b)| a.sub(b)),
                        &parts,
                    ),
                    BinOp::Mul => exact(
                        la.clone().zip(lb.clone()).and_then(|(a, b)| {
                            match (a.is_const(), b.is_const()) {
                                (true, _) => b.scale(a.c),
                                (_, true) => a.scale(b.c),
                                _ => None,
                            }
                        }),
                        &[],
                    ),
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
                let ge = |l: Option<Lin>| l.map(Fact::Ge);
                let (t, f) = match o.compare() {
                    // `x > y` is `x - y - 1 >= 0`, and `x >= y` is `x - y >= 0`.
                    Some(Cmp::Order { strict, flipped }) => {
                        let (x, y) = if flipped { (&lb, &la) } else { (&la, &lb) };
                        let s = i64::from(strict);
                        let holds = x.sub(y).and_then(|d| d.plus(-s));
                        (
                            vec![ge(holds)],
                            vec![ge(y.sub(x).and_then(|d| d.plus(s - 1)))],
                        )
                    }
                    Some(Cmp::Equal { negated }) => {
                        let both = vec![ge(la.sub(&lb)), ge(lb.sub(&la))];
                        let differ = vec![la.sub(&lb).map(Fact::Ne)];
                        if negated {
                            (differ, both)
                        } else {
                            (both, differ)
                        }
                    }
                    None => return self.bound(pre, n, *o, &la, &lb),
                };
                let norm = |fs: Vec<Option<Fact>>| {
                    fs.into_iter()
                        .flatten()
                        .filter_map(|f| f.map(|l| pre.norm(l)))
                        .collect()
                };
                Out::Cond(norm(t), norm(f))
            }
            (Op::Un(UnOp::Neg), [a]) => match exact(self.lin(a).and_then(|l| l.scale(-1)), &[]) {
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
        let both = |x: &Vec<Fact>, y: &Vec<Fact>| x.iter().chain(y).cloned().collect::<Vec<_>>();
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
            kind: Callee::Builtin | Callee::Reserved,
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
        let Rhs::Call { args, kind, .. } = rhs else {
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
        for (k, (a, cap)) in args.iter().enumerate() {
            match (a, cap) {
                (Arg::Place(Place::Field(r, f)), Capability::Modify | Capability::Consume) => {
                    if let Place::Name(r) = &**r {
                        self.resized_field(st, *r, f);
                    }
                }
                // A declared function proves the pairs of its `modify`
                // parameter's type at every exit. The argument's type may
                // have more pairs; then the walk tracks its lengths.
                (Arg::Val(Val::Name(r)) | Arg::Place(Place::Name(r)), Capability::Modify) => {
                    let theirs = match kind {
                        Callee::Fn(g) => self.sums.pairs.param(*g, k),
                        _ => None,
                    };
                    let kept = |a: &str, b: &str| {
                        theirs.is_some_and(|t| t.iter().any(|(x, y)| x == a && y == b))
                    };
                    match self.pairs_of(*r).iter().all(|(.., a, b)| kept(a, b)) {
                        true => self.restate(st, *r),
                        false => {
                            self.dirty.insert(*r);
                        }
                    }
                }
                _ => {}
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
        cands.extend(self.in_step(body, &stored));
        if !cands.is_empty() {
            let at_entry = entry.prover();
            cands.retain(|c| at_entry.ge0(c).is_some());
        }
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
            if cands.is_empty() {
                break;
            }
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

    /// `a - b` and its negation, where `a` and `b` are the lengths of two
    /// arrays or Strings the loop `body` stores to on the same paths, both of
    /// which a check can depend on: a loop that grows them in step keeps them
    /// as equal as it entered. [`Walk::settle`] keeps a candidate only where
    /// the entry and every turn prove it.
    fn in_step(&self, body: &[St], stored: &BTreeSet<Name>) -> Vec<Lin> {
        let seq = |n: &Name| self.slot[n.index()].relevant && self.kind(*n) == Kind::Seq;
        if stored.iter().filter(|n| seq(n)).count() < 2 {
            return Vec::new();
        }
        let mut paths = BTreeMap::new();
        store_paths(body, &mut Vec::new(), &mut paths);
        let seqs: Vec<(&Name, &Vec<Vec<u32>>)> = paths.iter().filter(|(n, _)| seq(n)).collect();
        let mut out = Vec::new();
        for (i, (a, pa)) in seqs.iter().enumerate() {
            for (b, pb) in &seqs[i + 1..] {
                if pa == pb {
                    let d = Lin::of(Term::Len(**a)).sub(&Lin::of(Term::Len(**b)));
                    out.extend(d.as_ref().and_then(|d| d.scale(-1)));
                    out.extend(d);
                }
            }
        }
        out
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
            (None, Guard::Clause { .. }) => [None, None],
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

/// Eliminates from `st`, a branch's end state, the names `binds` and the
/// names its rows `ss` bind with a `let` ([`State::eliminate`]), so a join
/// keeps what the branch knows through them about the names it shares.
fn leave(st: &mut State, ss: &[St], binds: &[Name]) {
    fn lets(ss: &[St], out: &mut BTreeSet<Name>) {
        for s in ss {
            match s {
                St::Let(n, _) => {
                    out.insert(*n);
                }
                St::Block { body, .. } => lets(body, out),
                _ => {}
            }
        }
    }
    if st.dead {
        return;
    }
    let mut names: BTreeSet<Name> = binds.iter().copied().collect();
    lets(ss, &mut names);
    for n in names {
        st.eliminate(n);
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

/// Per name of `body`, whether a goal can depend on it: a name a kept check
/// compares when `checks`, a parameter or a returned name when `returns` (a
/// summary's goals), argument `k` of a direct call to `g` where
/// `entering(g, k)` (a `pre`'s goals), and every name a definition, a
/// comparison, a check or a direct call to a `linked` callee links to a
/// relevant one, either way.
fn relevant(
    body: &Body,
    decls: &HashMap<String, TypeDecl>,
    ss: &[St],
    returns: bool,
    checks: bool,
    linked: &dyn Fn(FnId) -> bool,
    entering: &dyn Fn(FnId, usize) -> bool,
) -> Vec<bool> {
    let mut rel = vec![false; body.names.len()];
    for (s, _) in rows(ss) {
        let (St::Let(_, rhs) | St::Do { rhs, .. }) = s else {
            continue;
        };
        if let (Some(g), Rhs::Call { args, .. }) = (direct_call(rhs), rhs) {
            for (k, (a, _)) in args.iter().enumerate() {
                if let Some(n) = root(a).filter(|_| entering(g, k)) {
                    rel[n.index()] = true;
                }
            }
        }
    }
    if returns {
        for p in &body.params {
            rel[p.index()] = true;
        }
        for (s, _) in rows(ss) {
            if let St::Return {
                value: Some(Val::Name(n)),
                ..
            } = s
            {
                rel[n.index()] = true;
            }
        }
    }
    loop {
        let before = rel.iter().filter(|r| **r).count();
        mark(body, decls, ss, &mut rel, checks, linked);
        if rel.iter().filter(|r| **r).count() == before {
            return rel;
        }
    }
}

fn mark(
    body: &Body,
    decls: &HashMap<String, TypeDecl>,
    ss: &[St],
    rel: &mut [bool],
    checks: bool,
    linked: &dyn Fn(FnId) -> bool,
) {
    let val = |v: &Val| match v {
        Val::Name(n) => Some(*n),
        Val::Lit(_) => None,
    };
    for s in ss {
        match s {
            St::Check(c) => {
                // A kept check's names are relevant; any other check links them.
                let mut any = checks && c.verdict == Verdict::Kept;
                guard_names(&c.guard, &mut |n| any |= rel[n.index()]);
                if any {
                    guard_names(&c.guard, &mut |n| rel[n.index()] = true);
                }
            }
            St::Let(n, rhs) => {
                let n = Some(*n);
                match rhs {
                    Rhs::Val(v) => link(rel, n.into_iter().chain(val(v))),
                    Rhs::Read(p) | Rhs::Take(p) => {
                        let from = match p {
                            Place::Name(m) => Some(*m),
                            Place::Field(b, f) if f == "length" || f == "byteLength" => {
                                place_root(b)
                            }
                            Place::Field(b, _) if matches!(**b, Place::Name(_)) => place_root(p),
                            _ => None,
                        };
                        link(rel, n.into_iter().chain(from))
                    }
                    Rhs::Prim(_, vs, _) => {
                        link(rel, n.into_iter().chain(vs.iter().filter_map(val)))
                    }
                    // A literal defines its array and String fields' lengths.
                    Rhs::Make(Ctor::Record(..), vs) => {
                        let seq = |m: &Name| kind_of(&body.names[m.index()].ty, decls) == Kind::Seq;
                        link(
                            rel,
                            n.into_iter().chain(vs.iter().filter_map(val).filter(seq)),
                        )
                    }
                    Rhs::Call {
                        callee, args, kind, ..
                    } => {
                        let lengths = matches!(kind, Callee::Builtin | Callee::Reserved)
                            && prelude::builtin(callee)
                                .is_some_and(|b| b.length != Length::Unknown);
                        if lengths || direct_call(rhs).is_some_and(linked) {
                            let from = args.iter().filter_map(|(a, _)| root(a));
                            link(rel, n.into_iter().chain(from));
                        }
                    }
                    _ => {}
                }
            }
            St::Store { place, value, .. } => {
                if let (Some(n), Some(m)) = (resized_by_store(place), val(value)) {
                    if rel[n.index()] || rel[m.index()] {
                        rel[n.index()] = true;
                        rel[m.index()] = true;
                    }
                }
            }
            St::If {
                cond, then, els, ..
            } => {
                // A loop's exit condition carries the facts of its operands
                // when it is a Bool copy (`a && b`), for a check in the loop.
                if let (true, Some(c), [], [St::Break { .. }]) =
                    (checks, val(cond), &then[..], &els[..])
                {
                    rel[c.index()] = true;
                }
                mark(body, decls, then, rel, checks, linked);
                mark(body, decls, els, rel, checks, linked);
            }
            St::Loop { body: l, .. } | St::Block { body: l, .. } => {
                mark(body, decls, l, rel, checks, linked)
            }
            St::Switch { arms, .. } => {
                (arms.iter()).for_each(|a| mark(body, decls, &a.body, rel, checks, linked))
            }
            _ => {}
        }
    }
}

/// Hands `f` each name the check `g` compares.
fn guard_names(g: &Guard, f: &mut dyn FnMut(Name)) {
    let mut val = |v: &Val| {
        if let Val::Name(n) = v {
            f(*n)
        }
    };
    match g {
        Guard::Index(p, i) | Guard::Span(p, i, _) => {
            place_root(p).map(Val::Name).iter().for_each(&mut val);
            val(i);
        }
        Guard::Shift(k, _) | Guard::NonZero(k) | Guard::NoOverflow(_, k, _) => val(k),
        Guard::Range(..) => {}
        Guard::Rule(r) => val(&Val::Name(*r)),
        Guard::Clause { atoms, .. } => atoms.iter().for_each(|a| {
            val(&a.l.of);
            val(&a.r.of);
        }),
    }
}

/// Marks every name of `names` relevant when one of them is.
fn link(rel: &mut [bool], names: impl Iterator<Item = Name> + Clone) {
    if names.clone().any(|n| rel[n.index()]) {
        names.for_each(|n| rel[n.index()] = true);
    }
}

/// Per name the rows of `ss` store to, the path to each such store: the
/// index of each enclosing row, and of the branch or arm taken.
fn store_paths(ss: &[St], path: &mut Vec<u32>, out: &mut BTreeMap<Name, Vec<Vec<u32>>>) {
    for (i, s) in (0u32..).zip(ss) {
        path.push(i);
        match s {
            St::Store {
                place: Place::Name(n),
                ..
            } => out
                .entry(*n)
                .or_default()
                .push(path[..path.len() - 1].to_vec()),
            St::If { then, els, .. } => {
                for (k, b) in (0u32..).zip([then, els]) {
                    path.push(k);
                    store_paths(b, path, out);
                    path.pop();
                }
            }
            St::Loop { body, .. } | St::Block { body, .. } => store_paths(body, path, out),
            St::Switch { arms, .. } => {
                for (k, a) in (0u32..).zip(arms) {
                    path.push(k);
                    store_paths(&a.body, path, out);
                    path.pop();
                }
            }
            _ => {}
        }
        path.pop();
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
