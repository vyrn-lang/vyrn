//! Proves check rows ([`crate::check`]) that cannot fail, from what one body
//! states about its own names ([`crate::facts`]).
//!
//! [`decide`] walks a body's rows forward. A `let` or a store of an integer
//! defines its name; a comparison remembers the facts its truth and falsehood
//! give, a Bool copy and a join carry them, and an `if` on it assumes them; a
//! builtin's row states how it moves its receiver's length, a direct call's
//! whole `modify` argument moves as its callee's frame states ([`frames`]),
//! and any other `modify` or `consume` argument that is not a scalar forgets
//! its name. After a check row the path has its guard, since it traps
//! otherwise. A loop's head keeps the candidate facts that hold at
//! entry and after every turn (Houdini): each round drops at least one
//! candidate or stops, so the candidate count bounds the rounds.
//!
//! A direct call's result has the facts its callee's [`Summary`] states,
//! over the call's arguments ([`summaries`]); a call to a generic function
//! reads its instance's. A sentinel's facts hold where its integer result is
//! at least zero ([`Summary::when`]). Any other call's result is a fresh
//! value. A body that only direct call rows enter starts with the facts
//! every such row proves of its arguments. Every body starts with its
//! parameters' `where` clauses ([`Body::assumes`]), which every call row
//! checks; a call's clause check is a row like any other.
//!
//! The walk knows nothing about a global, an element, or a field of a field:
//! each read of one is a fresh value with its type's range. The length of
//! an element array an index name picks is one term until a row may write an
//! element ([`State::forget_inner`]). A check on a
//! global's array field is the exception: it reads the length of a borrow of
//! the global while the borrow is [`State::fresh`], up to a row the effect
//! judgment says may write the global ([`Walk::stale`]). An exact sum is an
//! `Int64` sum that provably stays in `-2^62..=2^62`. A row is proved only with
//! a certificate that [`crate::facts::Cert::verify`] accepts, or a divisor the
//! state holds unequal to zero.
//!
//! One postulate: by the kernel's exclusivity judgment no other name writes a
//! live name's source, so a name's length, and a record name's fields, change
//! only where the walk sees the name itself written.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use vyrn_frontend::ast::{BinOp, Capability, Field, FnId, Type, TypeDecl, UnOp};
use vyrn_frontend::effects::Effect;
use vyrn_frontend::prelude::{self, Elements, Length};
use vyrn_frontend::prim::Cmp;

use crate::effects::StateUse;
use crate::facts::{Fact, Lin, State, Term, EXACT};
use vyrn_frontend::core::check::{Atom, Check, Guard, Operand, Site, Verdict, Why};
use vyrn_frontend::core::{
    rows, Arg, Arm, Body, BorrowKind, Callee, Ctor, Lit, Name, Op, Place, Rhs, St, Target, Test,
    Val,
};
use vyrn_frontend::par::in_parallel;

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
    let mut w = Walk::new(body, decls, sums, &stmts, None, false);
    let st = w.entry(pre);
    w.block(st, &mut stmts);
    let refuted = w.refuted;
    body.stmts = stmts;
    refuted
}

/// What every entry and every return of a body state, over its interface:
/// in a fact, `Name(0)` is the result, or the payload of an `Option` of an
/// integer ([`Walk::payload`]), and `Term::Field(Name(0), i)` is integer
/// field `i` of an `Option` of a record's payload ([`payload_fields`]);
/// `Name(k + 1)` is parameter `k`; a
/// [`Term::Val`] is an integer's value and a [`Term::Len`] an array's or
/// String's length, and a [`Term::Field`] or [`Term::Col`] the same of a
/// record parameter's field, by its index in the record's fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// Per parameter, its kind; a call whose arguments differ states nothing.
    params: Vec<Kind>,
    /// Where a fact names a parameter's field, per parameter, its type: a
    /// field's index means the same field only in an argument of the same
    /// type. Empty otherwise.
    types: Vec<Type>,
    /// Facts over the parameters that every call row proves of its
    /// arguments before the call. Empty for a body anything else may enter.
    pre: BTreeSet<Lin>,
    /// Facts every return proves, whatever the arguments; for a Bool result,
    /// every return of `true`, which the caller assumes where the result is
    /// true. A fact names only parameters the body never writes, so it holds
    /// of the arguments as the caller passed them.
    post: BTreeSet<Lin>,
    /// For an integer result some return makes negative (a sentinel, as
    /// `-1` for "none"): the facts over the parameters every return of a
    /// value at least zero proves. The caller assumes them where the result
    /// is at least zero ([`Walk::sentinel`]).
    when: BTreeSet<Lin>,
}

/// Each summarized body's [`Summary`], keyed by its row in the World's
/// function table, never by a name.
#[derive(Debug, Default)]
pub struct Summaries {
    at: HashMap<FnId, usize>,
    instances: Instances,
    values: Vec<Summary>,
    /// Bodies [`summaries`] decided as [`decide`] decides them, each served
    /// once ([`Summaries::decided`]).
    decided: HashMap<FnId, Mutex<Option<Body>>>,
    facts: RecordFacts,
    /// What a call into each frame may do to module state, by the effect
    /// judgment ([`Walk::stale`]).
    state: Arc<HashMap<FnId, StateUse>>,
    /// What a call may do to each `modify` argument's lengths ([`frames`]).
    frames: Frames,
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

/// A part of a [`RecordFact`]: a field, by name, as an integer field's value,
/// an array or String field's length, or each element of an integer array
/// field; or the constant one. A fact with an `Elem` part holds of every
/// element ([`element_facts`]).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Part {
    Val(String),
    Len(String),
    Elem(String),
    One,
}

/// `sum(k * part) >= 0` over the fields of one record value, parts sorted.
type RecordFact = Vec<(Part, i64)>;

/// The sorted fact of `parts`.
fn record_fact(mut parts: RecordFact) -> RecordFact {
    parts.sort();
    parts
}

/// Whether a record with `fields` has every part of `fact`, each of its
/// part's kind.
fn binds(fact: &RecordFact, fields: &[Field], decls: &HashMap<String, TypeDecl>) -> bool {
    fact.iter().all(|(p, _)| {
        let (name, kind): (&str, fn(Kind) -> bool) = match p {
            Part::Val(f) => (f, Kind::is_int),
            Part::Len(f) => (f, |k| k == Kind::Seq),
            Part::Elem(f) => {
                return (fields.iter()).any(|x| x.name == *f && int_elements(&x.ty, decls));
            }
            Part::One => return true,
        };
        (fields.iter()).any(|x| x.name == name && kind(kind_of(&x.ty, decls)))
    })
}

/// The [`RecordFact`]s every record value with their fields keeps
/// ([`invariants`]): two array fields of equal length, an integer field at
/// least zero or one, an array or String field at least as long as an
/// integer field's value or longer.
#[derive(Debug, Default)]
pub struct RecordFacts {
    /// Every fact no round has dropped.
    of: BTreeSet<RecordFact>,
    /// Per record declaration, the facts it [`binds`].
    by_decl: HashMap<String, Vec<RecordFact>>,
    /// [`Records::params`].
    params: Arc<BTreeMap<(usize, usize), String>>,
}

impl RecordFacts {
    fn new(
        of: BTreeSet<RecordFact>,
        decls: &HashMap<String, TypeDecl>,
        params: &Arc<BTreeMap<(usize, usize), String>>,
    ) -> RecordFacts {
        let mut facts = RecordFacts {
            of,
            by_decl: HashMap::new(),
            params: params.clone(),
        };
        if facts.of.is_empty() {
            return facts;
        }
        for name in decls.keys() {
            let ty = Type::Named(name.clone());
            let Some(fields) = vyrn_frontend::types::record_fields(&ty, decls) else {
                continue;
            };
            let within: Vec<RecordFact> = (facts.within(&fields, decls).into_iter())
                .cloned()
                .collect();
            if !within.is_empty() {
                facts.by_decl.insert(name.clone(), within);
            }
        }
        facts
    }

    fn is_empty(&self) -> bool {
        self.of.is_empty()
    }

    /// Whether a fact bounds the elements of a field named `f`.
    fn bounds_elements_of(&self, f: &str) -> bool {
        (self.of.iter()).any(|x| x.iter().any(|(p, _)| matches!(p, Part::Elem(e) if e == f)))
    }

    /// The facts of the declared type of `modify` parameter `k` of `g`.
    fn param(&self, g: FnId, k: usize) -> Option<&[RecordFact]> {
        let d = self.params.get(&(g.index(), k))?;
        Some(self.by_decl.get(d).map_or(&[], Vec::as_slice))
    }

    /// The facts a record of type `ty` [`binds`].
    fn of_type(&self, ty: &Type, decls: &HashMap<String, TypeDecl>) -> Vec<&RecordFact> {
        if self.is_empty() {
            return Vec::new();
        }
        match ty {
            Type::Named(d) | Type::App(d, _) => self.by_decl.get(d).into_iter().flatten().collect(),
            _ => match vyrn_frontend::types::record_fields(ty, decls) {
                Some(fs) => self.within(&fs, decls),
                None => Vec::new(),
            },
        }
    }

    /// The facts a record with `fields` [`binds`].
    fn within(&self, fields: &[Field], decls: &HashMap<String, TypeDecl>) -> Vec<&RecordFact> {
        (self.of.iter())
            .filter(|f| binds(f, fields, decls))
            .collect()
    }
}

impl Summaries {
    /// The body of `f` as [`decide`] decides it, when [`summaries`] decided
    /// it and no caller took it before.
    pub fn decided(&self, f: FnId) -> Option<Body> {
        self.decided.get(&f)?.lock().ok()?.take()
    }
}

/// Per generic function's row, each instance's row under the type arguments
/// that spell it, by parameter name in the function's order: the body the
/// emitter runs for a call row whose `solved` they equal.
pub type Instances = HashMap<FnId, Vec<(Vec<(String, Type)>, FnId)>>;

/// [`Summaries`] borrowed: the solver's values while one body is walked.
#[derive(Clone, Copy)]
struct View<'a> {
    at: &'a HashMap<FnId, usize>,
    instances: &'a Instances,
    values: &'a [Summary],
    facts: &'a RecordFacts,
    state: &'a HashMap<FnId, StateUse>,
    frames: &'a Frames,
}

impl<'a> View<'a> {
    /// The summary of `f`, with its index.
    fn get(&self, f: FnId) -> Option<(usize, &Summary)> {
        let i = *self.at.get(&f)?;
        Some((i, self.values.get(i)?))
    }

    /// The row whose body `rhs` runs ([`instance`]).
    fn call(&self, rhs: &Rhs) -> Option<FnId> {
        instance(self.instances, rhs)
    }

    /// What the direct call `rhs` may do to its argument `k` ([`frames`]).
    fn frame(&self, rhs: &Rhs, k: usize) -> Option<&'a Frame> {
        self.frames.get(&(self.call(rhs)?, k))
    }

    /// Whether `f` states a fact of its result.
    fn post(&self, f: FnId) -> bool {
        self.get(f)
            .is_some_and(|(_, s)| !s.post.is_empty() || !s.when.is_empty())
    }

    /// Whether `rhs` is a direct call whose callee states a fact of its result.
    fn linked(&self, rhs: &Rhs) -> bool {
        self.call(rhs).is_some_and(|f| self.post(f))
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
        let (i, sum) = self.get(self.call(rhs)?)?;
        (!sum.pre.is_empty()).then_some((i, sum, args.as_slice()))
    }
}

impl<'a> From<&'a Summaries> for View<'a> {
    fn from(s: &'a Summaries) -> View<'a> {
        View {
            at: &s.at,
            instances: &s.instances,
            values: &s.values,
            facts: &s.facts,
            state: &s.state,
            frames: &s.frames,
        }
    }
}

/// The row whose body a direct call row runs ([`direct_call`]): an
/// instance's row for a call to a generic function. `None` for a call whose
/// `solved` no instance's type arguments equal.
fn instance(instances: &Instances, rhs: &Rhs) -> Option<FnId> {
    let g = direct_call(rhs)?;
    let Rhs::Call { solved, .. } = rhs else {
        return None;
    };
    if solved.is_empty() {
        return Some(g);
    }
    let mut of = instances.get(&g)?.iter();
    of.find(|(args, _)| args == solved).map(|(_, f)| *f)
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
/// [`entry_templates`], and the field facts its callees' candidates state of
/// its parameters ([`forward`]), as its `pre`, less each fact some call row
/// to it does not prove, its caller walked with every `post` ([`entries`]).
/// A caller whose `pre` holds an element template or a field fact is walked
/// under it, as a greatest fixpoint; every other caller is walked once,
/// without its own `pre`.
///
/// The record facts' walks assume [`parameter_facts`] at a body's entry. When
/// a body's `pre` leaves one out, the solve starts again without it.
///
/// The result does not depend on the order of `bodies`. Soundness rests on
/// two postulates: a [`Callee::Fn`] row runs the body `bodies` holds under
/// its row, or for a generic function the body of the row `instances` names
/// for its `solved`, and `bodies` holds no body for a row two bodies share or
/// for a generic function; and only a [`Callee::Fn`] row of a body in
/// `bodies`, or a row that spells its name, enters a body of `closed`.
///
/// Every walk assumes the [`RecordFacts`] that [`invariants`] keeps from
/// `records`; `None` when a body that runs may be missing from `bodies`, and
/// then no fact holds.
pub fn summaries<'a>(
    bodies: impl Iterator<Item = (FnId, &'a Body)>,
    decls: &HashMap<String, TypeDecl>,
    closed: &HashSet<FnId>,
    records: Option<&Records>,
    instances: Instances,
    state: Arc<HashMap<FnId, StateUse>>,
) -> Summaries {
    let mut bodies: Vec<(FnId, &Body)> = bodies.collect();
    bodies.sort_by_key(|(f, _)| f.index());
    let mut frames = frames(&bodies, decls, &instances);
    element_rows(&bodies, decls, &instances, &state, &mut frames);
    // A body's name count weighs its work.
    let weigh = |(_, b): &(FnId, &Body)| b.names.len();
    let firsts: Vec<(Vec<i64>, Option<Scan>, [Vec<Pair>; 2])> = in_parallel(
        &bodies,
        weigh,
        || (),
        |(), (_, b)| {
            let needs = needed_lengths(b, decls);
            let scan = records.map(|_| scan(b, decls, &needs));
            let pairs = [indexed_pairs(b, decls), made_pairs(b, decls)];
            // A result's candidate lengths start at two: one cost 0.2% more
            // `elide: summaries` allocations there and proved no more rows.
            let needs = needs.into_iter().map(|(_, c)| c).filter(|c| *c >= 2);
            (needs.collect(), scan, pairs)
        },
    );
    let mut needed = BTreeSet::new();
    let (mut indexed, mut made) = (BTreeSet::new(), BTreeSet::new());
    let mut scans = Vec::new();
    for (needs, scan, [i, m]) in firsts {
        needed.extend(needs);
        scans.push(scan);
        indexed.extend(i);
        made.extend(m);
    }
    // A pair a body indexes by and a body makes, either way round.
    let pairs: BTreeSet<Pair> = (indexed.into_iter())
        .filter(|(t, u)| {
            made.contains(&(t.clone(), u.clone())) || made.contains(&(u.clone(), t.clone()))
        })
        .collect();
    let values: Vec<Summary> = in_parallel(
        &bodies,
        weigh,
        || (),
        |(), (_, b)| templates(b, decls, &needed, &pairs),
    );
    // A record result's facts serve the call rows that prove a `pre`, which
    // no check of the caller seeds: solve them all.
    let records_out: BTreeSet<usize> = (0..values.len())
        .filter(|&i| {
            let result = |(t, _): &(Term, i64)| matches!(t, Term::Col(Name(0), _));
            values[i].post.iter().any(|f| f.terms.iter().any(result))
        })
        .collect();
    let mut at: HashMap<FnId, usize> = (0..).zip(&bodies).map(|(i, (f, _))| (*f, i)).collect();
    let scans: Option<Vec<Scan>> = scans.into_iter().collect();
    let (closed, callees) = entered(&bodies, closed);
    // The walks of [`invariants`] assume at a body's entry the facts it
    // returns of the body's parameters, and the body's `pre` must keep each
    // of them, or the solve starts again assuming only those it kept. Each
    // later start assumes fewer, so the starts are at most the facts plus one.
    let mut keep: Option<Vec<BTreeSet<Lin>>> = None;
    let mut first = Some(values);
    loop {
        let mut values = first.take().unwrap_or_else(|| {
            in_parallel(
                &bodies,
                weigh,
                || (),
                |(), (_, b)| templates(b, decls, &needed, &pairs),
            )
        });
        let (facts, assumed) = match (records, scans.as_ref()) {
            (Some(r), Some(scans)) => {
                // A record field's constant length comes from a callee's
                // result (`classOf: byteClasses(bd)`): solve those facts
                // first, with no record fact, for the walks that prove the
                // record's.
                let mut start: BTreeSet<usize> = match scans.iter().any(|s| !s.lengths.is_empty()) {
                    true => (scans.iter().flat_map(|s| &s.fills))
                        .filter_map(|g| at.get(g).copied())
                        .filter(|&i| values[i].post.iter().any(constant_length))
                        .collect(),
                    false => BTreeSet::new(),
                };
                // A sentinel's facts let a writer of a record prove its facts.
                start.extend((0..values.len()).filter(|&i| !values[i].when.is_empty()));
                let lengths = match start.is_empty() {
                    true => Vec::new(),
                    false => {
                        let only = (values.iter())
                            .map(|v| Summary {
                                params: v.params.clone(),
                                types: v.types.clone(),
                                pre: BTreeSet::new(),
                                when: v.when.clone(),
                                post: v
                                    .post
                                    .iter()
                                    .filter(|f| constant_length(f))
                                    .cloned()
                                    .collect(),
                            })
                            .collect();
                        solve(
                            &bodies,
                            decls,
                            &at,
                            &instances,
                            only,
                            start,
                            &RecordFacts::default(),
                            &state,
                            &frames,
                        )
                    }
                };
                let view = View {
                    at: &at,
                    instances: &instances,
                    values: &lengths,
                    facts: &RecordFacts::default(),
                    state: &state,
                    frames: &frames,
                };
                invariants(&bodies, scans, decls, r, view, &closed, keep.as_deref())
            }
            _ => (RecordFacts::default(), vec![BTreeSet::new(); bodies.len()]),
        };
        let view = View {
            at: &at,
            instances: &instances,
            values: &values,
            facts: &facts,
            state: &state,
            frames: &frames,
        };
        let start: BTreeSet<usize> =
            in_parallel(&bodies, weigh, || (), |(), (_, b)| seeds(b, decls, view))
                .into_iter()
                .flatten()
                .chain(records_out.iter().copied())
                .collect();
        values = solve(
            &bodies, decls, &at, &instances, values, start, &facts, &state, &frames,
        );
        let view = View {
            at: &at,
            instances: &instances,
            values: &values,
            facts: &facts,
            state: &state,
            frames: &frames,
        };
        let pres = in_parallel(
            &bodies,
            weigh,
            || (),
            |(), (f, b)| {
                let mine = &assumed[at[f]];
                match closed.contains(f) && (any_check(&b.stmts) || !mine.is_empty()) {
                    true => entry_templates(b, decls, view, &pairs, mine),
                    false => (BTreeSet::new(), None),
                }
            },
        );
        let mut decided = Vec::new();
        for (i, (pre, d0)) in pres.into_iter().enumerate() {
            values[i].pre = pre;
            decided.extend(d0.map(|d| (i, d)));
        }
        forward(&bodies, decls, &closed, &at, &instances, &mut values);
        // Every caller of a body with a `pre` proves it at each call row, or
        // the body loses the facts it does not. Lighter callers go first: a
        // `pre` they empty spares the heavier callers' walks. The
        // intersection does not depend on the order. Each batch takes two
        // thirds of what is left.
        let mut callers: Vec<&Body> = (bodies.iter().zip(&callees))
            .filter(|(_, gs)| gs.iter().any(|g| !values[at[g]].pre.is_empty()))
            .map(|((_, b), _)| *b)
            .collect();
        // A caller whose own `pre` holds element templates, or a fact over
        // a record parameter's field, is walked under it below: a fact of
        // its callee may rest on one of its own.
        let templated = |i: usize| {
            let seq = |j: usize| {
                j.checked_sub(1)
                    .is_some_and(|k| values[i].params.get(k) == Some(&Kind::Seq))
            };
            values[i]
                .pre
                .iter()
                .any(|f| hole(f, seq).is_some() || names_field(f))
        };
        let under: Vec<(usize, &Body)> = (callers.iter())
            .filter_map(|b| Some((at[&b.id?], *b)))
            .filter(|(i, _)| templated(*i))
            .collect();
        callers.retain(|b| !under.iter().any(|(_, u)| std::ptr::eq(*u, *b)));
        callers.sort_by_key(|b| b.names.len());
        while !callers.is_empty() {
            let view = View {
                at: &at,
                instances: &instances,
                values: &values,
                facts: &facts,
                state: &state,
                frames: &frames,
            };
            callers.retain(|b| rows(&b.stmts).any(|(s, _)| view.pre(s).is_some()));
            let batch: Vec<&Body> = callers.drain(..(callers.len() * 2).div_ceil(3)).collect();
            let proved = in_parallel(
                &batch,
                |b| b.names.len(),
                || (),
                |(), b| entries(b, decls, view, &BTreeSet::new()),
            );
            for (g, facts) in proved.into_iter().flatten() {
                values[g].pre.retain(|f| facts.contains(f));
            }
        }
        // The greatest fixpoint over the callers walked under their own
        // `pre`, from the facts left: each round walks every one under the
        // `pre` the round before left, and a round that goes on drops a
        // fact, so the rounds are at most the facts. Sound by induction on
        // the depth of a frame: a frame's caller entered under its own `pre`,
        // which proved the facts at the call row.
        let mut changed = !under.is_empty();
        while changed {
            let view = View {
                at: &at,
                instances: &instances,
                values: &values,
                facts: &facts,
                state: &state,
                frames: &frames,
            };
            let proved = in_parallel(
                &under,
                |(_, b)| b.names.len(),
                || (),
                |(), (i, b)| entries(b, decls, view, &values[*i].pre),
            );
            changed = false;
            for (g, facts) in proved.into_iter().flatten() {
                let before = values[g].pre.len();
                values[g].pre.retain(|f| facts.contains(f));
                changed |= values[g].pre.len() != before;
            }
        }
        // The record facts rest on what their walks assumed at entry: each
        // such fact must be in its body's `pre`. Sound by induction on the
        // steps of a run: a record is built after its body's entry, and that
        // entry's call row proved the `pre` under the record facts of the
        // steps before.
        let kept: Vec<BTreeSet<Lin>> = (assumed.iter().zip(&values))
            .map(|(a, v)| a.intersection(&v.pre).cloned().collect())
            .collect();
        if kept != assumed {
            keep = Some(kept);
            continue;
        }
        // A body decided without a `pre` is decided as an emitter would
        // decide it when it keeps none.
        let decided = (decided.into_iter())
            .filter(|(i, _)| values[*i].pre.is_empty())
            .map(|(i, d)| (bodies[i].0, Mutex::new(Some(d))))
            .collect();
        at.retain(|_, i| {
            let v = &values[*i];
            !v.pre.is_empty() || !v.post.is_empty() || !v.when.is_empty()
        });
        return Summaries {
            at,
            instances,
            values,
            decided,
            facts,
            state,
            frames,
        };
    }
}

/// Whether `f` names the length of a record's array field.
fn names_field(f: &Lin) -> bool {
    (f.terms.iter()).any(|(t, _)| matches!(t, Term::Col(..)))
}

/// Adds to the `pre` of each body only call rows enter ([`entered`]) each
/// fact over the lengths of record fields alone that a callee's `pre`
/// states of the arguments, where each argument it names is a parameter of
/// the body passed whole, of the callee's parameter's type: the body can
/// prove it at the call only from its own entry. Sets the parameter types of a
/// body whose `pre` names a field, so a call row maps the field
/// ([`Walk::field_arg`]).
///
/// Each round adds a fact or ends the loop. A fact added is a callee's with
/// its parameters renamed to the body's, and a body has finitely many
/// renamings of the finitely many seeded facts, so the rounds are bounded.
fn forward(
    bodies: &[(FnId, &Body)],
    decls: &HashMap<String, TypeDecl>,
    closed: &HashSet<FnId>,
    at: &HashMap<FnId, usize>,
    instances: &Instances,
    values: &mut [Summary],
) {
    let ty = |i: usize, k: usize| {
        let b = bodies[i].1;
        vyrn_frontend::types::resolved(&b.names[b.params[k].index()].ty, decls)
    };
    let mut added = true;
    while added {
        added = false;
        for (i, (f, b)) in bodies.iter().enumerate() {
            if !closed.contains(f) {
                continue;
            }
            let mut more = BTreeSet::new();
            for (s, _) in rows(&b.stmts) {
                let (St::Let(_, rhs) | St::Do { rhs, .. }) = s else {
                    continue;
                };
                let Rhs::Call { args, .. } = rhs else {
                    continue;
                };
                let Some(&g) = instance(instances, rhs).and_then(|g| at.get(&g)) else {
                    continue;
                };
                let rename = |t: Term| {
                    let Term::Col(n, x) = t else { return None };
                    let k = n.index().checked_sub(1)?;
                    let (Arg::Val(Val::Name(m)) | Arg::Place(Place::Name(m))) = &args.get(k)?.0
                    else {
                        return None;
                    };
                    let j = b.params.iter().position(|p| p == m)?;
                    (ty(g, k) == ty(i, j)).then(|| Lin::of(Term::Col(Name(j as u32 + 1), x)))
                };
                let facts = values[g].pre.iter().filter(|f| names_field(f));
                more.extend(facts.filter_map(|f| f.map(rename)));
            }
            more.retain(|f| !values[i].pre.contains(f));
            if !more.is_empty() {
                added = true;
                values[i].pre.extend(more);
            }
        }
    }
    for (i, (_, b)) in bodies.iter().enumerate() {
        if values[i].types.is_empty() && values[i].pre.iter().any(names_field) {
            values[i].types = (b.params.iter())
                .map(|p| b.names[p.index()].ty.clone())
                .collect();
        }
    }
}

/// How a call may move a slot of a `modify` argument: a record's array or
/// String field's length or integer field's value, or an array's or String's
/// length. Ordered from the least change up, so a join is `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Moves {
    Keeps,
    /// The length never falls.
    Grows,
    Any,
}

impl Moves {
    /// How a builtin row that states `len` moves its receiver.
    fn of(len: Length) -> Moves {
        match len {
            Length::Keeps => Moves::Keeps,
            Length::GrowsByOne | Length::GrowsByLenOf(_) => Moves::Grows,
            _ => Moves::Any,
        }
    }
}

/// What a call to one body may do to the argument of one parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Frame {
    /// For a `modify` record, array or String: its slots' [`Moves`], one per
    /// field of a record by the field's index, or one for an array's or
    /// String's length. Empty for any other parameter.
    moves: Vec<Moves>,
    /// For an index array ([`index_elements`]): every element of the result lies between the
    /// least and the greatest element the argument held at entry
    /// ([`element_rows`]).
    result: bool,
    /// For a `modify` index array: the same of every element the argument
    /// holds at exit.
    own: bool,
}

/// Per body's row and parameter, its [`Frame`].
type Frames = HashMap<(FnId, usize), Frame>;

/// The [`Frames`] of every `modify` parameter of `bodies` that is a record,
/// an array or a String: the least fixpoint of [`frame`] over the call
/// graph, from every slot kept. A slot rises at most twice, and a round that
/// raises none ends the solve.
///
/// Sound by the postulate [`summaries`] states of a [`Callee::Fn`] row and
/// the kernel's exclusivity judgment: a slot of the argument changes only
/// where a row of the callee writes through the parameter or a second name
/// for it, and [`frame`] reads every such row. A call to a row with no body
/// here moves every slot.
fn frames(
    bodies: &[(FnId, &Body)],
    decls: &HashMap<String, TypeDecl>,
    instances: &Instances,
) -> Frames {
    let mut out = Frames::new();
    for (f, b) in bodies {
        for (k, p) in b.params.iter().enumerate() {
            let moves = vec![Moves::Keeps; slots(b, *p, decls).unwrap_or(0)];
            let row = index_elements(&b.names[p.index()].ty, decls);
            if !moves.is_empty() || row {
                let frame = Frame {
                    result: row && returns_index_array(b, decls),
                    own: row && !moves.is_empty(),
                    moves,
                };
                out.insert((*f, k), frame);
            }
        }
    }
    let mut raised = !out.is_empty();
    while raised {
        raised = false;
        for (f, b) in bodies {
            for (k, p) in b.params.iter().enumerate() {
                let Some(mut now) = (out.get(&(*f, k)))
                    .map(|fr| fr.moves.clone())
                    .filter(|m| !m.is_empty())
                else {
                    continue;
                };
                {
                    let out = &out;
                    let callee = |rhs: &Rhs, i: usize| {
                        Some(out.get(&(instance(instances, rhs)?, i))?.moves.as_slice())
                    };
                    frame(b, *p, decls, &callee, &mut now);
                }
                let fr = out.get_mut(&(*f, k)).expect("read above");
                if now != fr.moves {
                    raised = true;
                    fr.moves = now;
                }
            }
        }
    }
    out
}

/// How many slots parameter `p` of `b` has when it is a `modify` parameter:
/// one per field of a record, one for an array or String. `None` for any
/// other parameter.
fn slots(b: &Body, p: Name, decls: &HashMap<String, TypeDecl>) -> Option<usize> {
    let info = &b.names[p.index()];
    if !info.is_modify_param() {
        return None;
    }
    match kind_of(&info.ty, decls) {
        Kind::Seq => Some(1),
        Kind::Other => vyrn_frontend::types::record_fields(&info.ty, decls).map(|fs| fs.len()),
        _ => None,
    }
}

/// Where a place lies in a `modify` parameter: the parameter itself, one of
/// its slots, or a part inside the parameter's value (an element, a field of
/// a field), whose store keeps every slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    Whole,
    Slot(usize),
    Inside,
}

/// Joins into `out` how the rows of `b` may move the slots of its `modify`
/// parameter `p`, taking a direct call's from `callee`, by the call and the
/// argument's index. A row moves a slot when it writes it through `p` or a
/// second name for `p`: a store, a take, a `modify` or `consume` argument. A
/// builtin's row written back (`let t = take p.f`, `let v = @push(consume t,
/// ..)`, `p.f = v`) moves it as the row states; a store inside it keeps it;
/// any other write moves it any way.
fn frame<'o>(
    b: &Body,
    p: Name,
    decls: &HashMap<String, TypeDecl>,
    callee: &dyn Fn(&Rhs, usize) -> Option<&'o [Moves]>,
    out: &mut [Moves],
) {
    let source = &b.names[p.index()].source;
    let alias = |n: Name| {
        matches!(&b.names[n.index()].borrow_kind,
            Some(BorrowKind::Param { cap: "modify", of, .. }) if of == source)
    };
    let fields = vyrn_frontend::types::record_fields(&b.names[p.index()].ty, decls);
    // `pl` as the parameter itself or one of its slots.
    let own = |pl: &Place| match pl {
        Place::Name(n) => alias(*n).then_some(Hit::Whole),
        Place::Field(r, f) if matches!(**r, Place::Name(n) if alias(n)) => {
            Some(match fields.as_ref() {
                Some(fs) => fs
                    .iter()
                    .position(|x| x.name == *f)
                    .map_or(Hit::Whole, Hit::Slot),
                None => Hit::Whole,
            })
        }
        _ => None,
    };
    // A store at a map's key may add an entry: it writes the map.
    let hit = |pl: &Place| {
        let at = match pl {
            Place::Key(r, _) => own(r),
            _ => own(pl),
        };
        at.or_else(|| place_root(pl).filter(|n| alias(*n)).map(|_| Hit::Inside))
    };
    let arg_hit = |a: &Arg| match a {
        Arg::Val(Val::Name(n)) => alias(*n).then_some(Hit::Whole),
        Arg::Place(pl) => hit(pl),
        Arg::Val(Val::Lit(_)) => None,
    };
    let builtin = |rhs: &Rhs| {
        matches!(
            rhs,
            Rhs::Call {
                kind: Callee::Builtin | Callee::Reserved,
                ..
            }
        )
    };
    // How a store of `v` into `h` moves it, when `v` is bound once to a
    // builtin's row whose receiver it consumes: `h` itself, or a name bound
    // once to a take of `h` and read nowhere else. With that name.
    let written_back = |v: Name, h: Hit| -> Option<(Moves, Option<Name>)> {
        let rhs = let_of(&b.stmts, v).filter(|r| builtin(r) && writes_of(&b.stmts, v) == 1);
        let Some(Rhs::Call { callee, args, .. }) = rhs else {
            return None;
        };
        let (a0, Capability::Consume) = args.first()? else {
            return None;
        };
        let m = Moves::of(length_rule(callee, args));
        if arg_hit(a0) == Some(h) {
            return Some((m, None));
        }
        let (Arg::Val(Val::Name(t)) | Arg::Place(Place::Name(t))) = a0 else {
            return None;
        };
        let Some(Rhs::Take(pl)) = let_of(&b.stmts, *t) else {
            return None;
        };
        (writes_of(&b.stmts, *t) == 2 && hit(pl) == Some(h)).then_some((m, Some(*t)))
    };
    let record = fields.is_some();
    let raise = |out: &mut [Moves], h: Hit, m: Moves| match (h, record) {
        (Hit::Slot(i), _) => out[i] = out[i].max(m),
        // A record replaced whole moves every slot.
        (Hit::Whole, true) => out.fill(Moves::Any),
        (Hit::Whole, false) => out[0] = out[0].max(m),
        (Hit::Inside, _) => {}
    };
    let mut takers: Vec<Name> = Vec::new();
    let mut results: Vec<Name> = Vec::new();
    for (s, _) in rows(&b.stmts) {
        let St::Store {
            place,
            value: Val::Name(v),
            ..
        } = s
        else {
            continue;
        };
        let back = hit(place)
            .filter(|h| *h != Hit::Inside)
            .and_then(|h| Some((h, written_back(*v, h)?)));
        if let Some((h, (m, t))) = back {
            raise(out, h, m);
            results.push(*v);
            takers.extend(t);
        }
    }
    for (s, _) in rows(&b.stmts) {
        match s {
            St::Store {
                value: Val::Name(v),
                ..
            } if results.contains(v) => {}
            St::Store { place, .. } => {
                if let Some(h) = hit(place) {
                    raise(out, h, Moves::Any);
                }
            }
            St::Let(n, Rhs::Take(pl)) if !takers.contains(n) => {
                if let Some(h) = hit(pl) {
                    raise(out, h, Moves::Any);
                }
            }
            St::Let(_, rhs @ Rhs::Call { args, .. })
            | St::Do {
                rhs: rhs @ Rhs::Call { args, .. },
                ..
            } => {
                let own = matches!(s, St::Let(n, _) if results.contains(n));
                for (i, (a, cap)) in args.iter().enumerate() {
                    let Some(h) = arg_hit(a).filter(|_| *cap != Capability::Read) else {
                        continue;
                    };
                    let m = match (h, cap) {
                        (Hit::Inside, _) => continue,
                        (Hit::Whole, Capability::Consume) if own && i == 0 => continue,
                        (_, Capability::Modify) if builtin(rhs) && i == 0 => {
                            let Rhs::Call { callee, .. } = rhs else {
                                continue;
                            };
                            Moves::of(length_rule(callee, args))
                        }
                        (Hit::Whole, Capability::Modify) if !builtin(rhs) => {
                            match callee(rhs, i).filter(|t| t.len() == out.len()) {
                                Some(theirs) => {
                                    for (o, t) in out.iter_mut().zip(theirs) {
                                        *o = (*o).max(*t);
                                    }
                                    continue;
                                }
                                None => Moves::Any,
                            }
                        }
                        _ => Moves::Any,
                    };
                    raise(out, h, m);
                }
            }
            St::Drop(n, ..) | St::Row { name: n, .. } if alias(*n) => {
                raise(out, Hit::Whole, Moves::Any)
            }
            St::Switch {
                on: Val::Name(n),
                consuming: true,
                ..
            } if alias(*n) => raise(out, Hit::Whole, Moves::Any),
            _ => {}
        }
    }
}

/// The names whose element templates a summary may state, for
/// [`relevant`]: each array parameter of the body's own `pre`, each integer
/// array the body returns when its `post` candidates hold templates, and at
/// each direct call row the array arguments a callee's `pre` or rows
/// ([`Frame`]) name and the result its `post` or a `result` row bounds.
fn template_roots(
    b: &Body,
    decls: &HashMap<String, TypeDecl>,
    ss: &[St],
    sums: View<'_>,
    pre: Option<&BTreeSet<Lin>>,
    post: bool,
) -> Vec<Name> {
    let mut out = Vec::new();
    let seq_param = |j: usize| {
        let p = j.checked_sub(1).and_then(|k| b.params.get(k));
        p.is_some_and(|p| kind_of(&b.names[p.index()].ty, decls) == Kind::Seq)
    };
    for f in pre.into_iter().flatten() {
        out.extend(hole(f, seq_param).map(|j| b.params[j.index() - 1]));
    }
    each_row(ss, &mut |s| match s {
        St::Return {
            value: Some(Val::Name(r)),
            ..
        } if post && index_elements(&b.names[r.index()].ty, decls) => out.push(*r),
        St::Let(_, rhs @ Rhs::Call { args, .. })
        | St::Do {
            rhs: rhs @ Rhs::Call { args, .. },
            ..
        } => {
            let Some(g) = sums.call(rhs) else { return };
            let sum = sums.get(g).map(|(_, s)| s);
            // `Val(Name(0))` is an element only of an array result.
            let result = matches!(s, St::Let(n, _) if index_elements(&b.names[n.index()].ty, decls))
                && sum.is_some_and(|s| s.post.iter().any(|l| l.coef(Term::Val(Name(0))) != 0));
            let mut any = result;
            for (k, (a, _)) in args.iter().enumerate() {
                let row = sums.frame(rhs, k).is_some_and(|f| f.result || f.own);
                let named = sum.is_some_and(|s| {
                    s.params.get(k) == Some(&Kind::Seq)
                        && s.pre
                            .iter()
                            .any(|l| l.coef(Term::Val(Name(k as u32 + 1))) != 0)
                });
                if row || named {
                    out.extend(root(a));
                    any |= row;
                }
            }
            if let (true, St::Let(n, _)) = (any, s) {
                out.push(*n);
            }
        }
        _ => {}
    });
    out
}

/// Calls `f` on every row of `ss` and every row under it, in program order,
/// as [`rows`] yields them, without the stack [`rows`] allocates: a walk
/// scans its body this way each time it starts.
fn each_row<'s>(ss: &'s [St], f: &mut dyn FnMut(&'s St)) {
    for s in ss {
        f(s);
        s.lists().for_each(|l| each_row(l, f));
    }
}

/// The interface name whose element the summary fact `l` bounds: the one
/// whose value it names among those `seq` marks as arrays. Such a fact is
/// an element template with that value as its hole ([`State::elems`]).
fn hole(l: &Lin, seq: impl Fn(usize) -> bool) -> Option<Name> {
    l.terms.iter().find_map(|(t, _)| match t {
        Term::Val(n) if seq(n.index()) => Some(*n),
        _ => None,
    })
}

/// The index array names an element of which a row of `ss` passes to a
/// direct call (`k = a[0]` before `flip(a, k)`): the callee's `pre` may bound
/// the argument by the array's length. A name a `let` binds to another
/// (`let b = read a`, `let a = p`) reads as that one, and a name an element
/// is stored into holds the element.
fn elements_passed(b: &Body, decls: &HashMap<String, TypeDecl>, ss: &[St]) -> BTreeSet<Name> {
    let mut alias: BTreeMap<Name, Name> = BTreeMap::new();
    let mut element: BTreeMap<Name, Name> = BTreeMap::new();
    for (s, _) in rows(ss) {
        let of = |a: Name| alias.get(&a).copied().unwrap_or(a);
        match s {
            St::Let(
                n,
                Rhs::Read(Place::Name(a)) | Rhs::Take(Place::Name(a)) | Rhs::Val(Val::Name(a)),
            ) => _ = alias.insert(*n, of(*a)),
            St::Let(n, Rhs::Read(Place::Elem(a, _))) => {
                if let Place::Name(a) = **a {
                    element.insert(*n, of(a));
                }
            }
            St::Store {
                place: Place::Name(k),
                value: Val::Name(v),
                ..
            } => {
                if let Some(a) = element.get(v).copied() {
                    element.insert(*k, a);
                }
            }
            _ => {}
        }
    }
    let mut out = BTreeSet::new();
    for (s, _) in rows(ss) {
        if let St::Let(_, rhs @ Rhs::Call { args, .. })
        | St::Do {
            rhs: rhs @ Rhs::Call { args, .. },
            ..
        } = s
        {
            if direct_call(rhs).is_some() {
                out.extend(
                    args.iter()
                        .filter_map(|(a, _)| element.get(&root(a)?).copied()),
                );
            }
        }
    }
    out.retain(|a| index_elements(&b.names[a.index()].ty, decls));
    out
}

/// Whether some return of `b` hands back an index array ([`index_elements`]).
fn returns_index_array(b: &Body, decls: &HashMap<String, TypeDecl>) -> bool {
    rows(&b.stmts).any(|(s, _)| {
        matches!(s, St::Return { value: Some(Val::Name(n)), .. }
            if index_elements(&b.names[n.index()].ty, decls))
    })
}

/// The ghost templates of parameter `k`, the index array `p`: every
/// element at most `Ghost(k, true)` and at least `Ghost(k, false)`.
fn ghost_pair(k: usize, p: Name) -> [Lin; 2] {
    let e = Lin::of(Term::Val(p));
    let bound = |upper| Lin::of(Term::Ghost(k as u32, upper));
    [
        bound(true).sub(&e).expect("two terms"),
        e.sub(&bound(false)).expect("two terms"),
    ]
}

/// Weakens the `result` and `own` rows of `frames` to what each body's
/// walk proves ([`Frame`]): the greatest fixpoint from every row holding,
/// each walk taking its callees' rows from the round before. A round that
/// weakens none ends the solve, and a row weakens at most once, so the
/// rounds are at most the rows.
///
/// A walk starts each index array parameter `p` with the templates
/// `G - e >= 0` and `e - H >= 0` over two ghosts, terms no fact names and
/// no write moves. A value meets `G - v >= 0` only by a chain to an element
/// read from such an array, so a stored value lies between the least and
/// the greatest element `p` held at entry. The ghosts are universal, so
/// every template of `p`, one linear bound on its element, holds of the
/// value; an empty `p` yields no element, because the row admits only
/// values read from it. Sound by induction on the depth of a call that
/// returns: the calls it makes return at a lesser depth, so their rows hold
/// where the walk reads them. A call that does not return claims nothing.
fn element_rows(
    bodies: &[(FnId, &Body)],
    decls: &HashMap<String, TypeDecl>,
    instances: &Instances,
    state: &HashMap<FnId, StateUse>,
    frames: &mut Frames,
) {
    let (at, values, facts) = (HashMap::new(), Vec::new(), RecordFacts::default());
    loop {
        let live = |(f, b): &&(FnId, &Body)| {
            (0..b.params.len()).any(|k| frames.get(&(*f, k)).is_some_and(|r| r.result || r.own))
        };
        let todo: Vec<&(FnId, &Body)> = bodies.iter().filter(live).collect();
        let view = View {
            at: &at,
            instances,
            values: &values,
            facts: &facts,
            state,
            frames,
        };
        let found: Vec<(FnId, (Vec<bool>, Vec<bool>))> = (todo.iter())
            .map(|(f, b)| (*f, rows_of(b, decls, view)))
            .collect();
        let mut weakened = false;
        for (f, (result, own)) in found {
            for k in 0..result.len() {
                if let Some(r) = frames.get_mut(&(f, k)) {
                    let (a, b) = (r.result && result[k], r.own && own[k]);
                    weakened |= (a, b) != (r.result, r.own);
                    (r.result, r.own) = (a, b);
                }
            }
        }
        if !weakened {
            return;
        }
    }
}

/// Per parameter of `body`, whether every return holds its `result` and its
/// `own` row, walking it with the ghosts and the callees' rows `sums` holds.
fn rows_of(
    body: &Body,
    decls: &HashMap<String, TypeDecl>,
    sums: View<'_>,
) -> (Vec<bool>, Vec<bool>) {
    let mut stmts = body.stmts.clone();
    let mut w = Walk::new(body, decls, sums, &stmts, Some(None), true);
    let n = body.params.len();
    w.rows = Some((vec![true; n], vec![true; n]));
    let st = w.entry(&BTreeSet::new());
    // A body that falls off its end returns there.
    let end = w.block(st, &mut stmts);
    w.returned(&end, None);
    w.rows.expect("set above")
}

/// Whether `f` is a summary's `len(r) - c >= 0`.
fn constant_length(f: &Lin) -> bool {
    f.terms == [(Term::Len(Name(0)), 1)]
}

/// The `post` of each body of `values` that every return proves, the
/// greatest fixpoint ([`crate::fixpoint::descend`]) from the candidates
/// `values` holds, visiting the bodies of `start` and each callee a visited
/// body with facts left took facts from. Each walk assumes `facts`. A body
/// no visit reached keeps no `post`.
fn solve(
    bodies: &[(FnId, &Body)],
    decls: &HashMap<String, TypeDecl>,
    at: &HashMap<FnId, usize>,
    instances: &Instances,
    values: Vec<Summary>,
    start: BTreeSet<usize>,
    facts: &RecordFacts,
    state: &HashMap<FnId, StateUse>,
    frames: &Frames,
) -> Vec<Summary> {
    let mut seen = vec![false; bodies.len()];
    start.iter().for_each(|&i| seen[i] = true);
    // Per body, the bodies whose facts its facts went into: visited again
    // when it loses one. Every reader of a lowered callee is revisited: a
    // reader that registers after its callee's first visit may have read a
    // value that an earlier update of the same round has since lowered.
    let mut readers = vec![BTreeSet::new(); bodies.len()];
    let walk = |i: usize, values: &[Summary]| {
        let view = View {
            at,
            instances,
            values,
            facts,
            state,
            frames,
        };
        returns(bodies[i].1, decls, view, &values[i])
    };
    let weight = |i: usize| bodies[i].1.names.len();
    let mut values =
        crate::fixpoint::descend(values, start, weight, walk, |i, (kept, read), values| {
            let mut next = Vec::new();
            let (kept, when) = kept;
            if kept.len() < values[i].post.len() || when.len() < values[i].when.len() {
                next.extend(readers[i].iter().copied());
            }
            values[i].post = kept;
            values[i].when = when;
            // A body with no facts left reads no callee's.
            if !values[i].post.is_empty() || !values[i].when.is_empty() {
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
            v.when.clear();
        }
    }
    values
}

/// The [`RecordFacts`] every record value of the program keeps, by Houdini.
/// The candidates come from each declared record type: each two array fields
/// of equal length when a check indexes one; each integer field a body reads
/// of a record name whose array or String field a check indexes, at least
/// zero and at most that field's length; each array or String field a check
/// indexes by a constant bound ([`needed_lengths`]), at least that long; each
/// integer field a body reads into a divisor, at least one.
///
/// Record types are compatible by shape, so a fact is keyed by its field
/// names and binds every record value with those fields, whatever its type.
/// A walk assumes a record name's facts where the name takes a value it did
/// not build ([`Walk::restate`]). It tracks the fields of a record that a
/// literal builds or a row writes a field of ([`Walk::dirty`]), and that
/// record proves its facts wherever its value leaves the name
/// ([`Walk::keeps`]). The bodies that build or write a record with a fact
/// are queued. Each batch walks the lightest two thirds of the queue and drops
/// each fact a body does not prove, so a fact a light body drops spares the
/// heavier bodies' walks. A body whose last walk assumed a dropped fact is
/// queued again, and one that builds or writes no record with a fact left
/// leaves the queue. A batch drops a fact or shortens the queue, so the two
/// bound the batches; the facts kept do not depend on the order.
///
/// Before the first batch, a fact goes when a host-made type binds it, or
/// when a row writes one of its fields through a place the walk does not
/// track: a global, an element, a nested field, or a name whose type is not a
/// record.
///
/// A walk takes a direct call's result facts from `lengths`, the constant
/// lengths solved with no record fact. A walk of a body of `closed` starts
/// with [`parameter_facts`] as its `pre`, less those `keep` leaves out; the
/// facts each walked body assumed are returned beside the record facts, for
/// [`summaries`] to hold the body's `pre` to them.
fn invariants(
    bodies: &[(FnId, &Body)],
    scans: &[Scan<'_>],
    decls: &HashMap<String, TypeDecl>,
    records: &Records,
    lengths: View<'_>,
    closed: &HashSet<FnId>,
    keep: Option<&[BTreeSet<Lin>]>,
) -> (RecordFacts, Vec<BTreeSet<Lin>>) {
    let host = &records.host;
    let resolve = |t: &Type| vyrn_frontend::types::resolve(t, decls);
    let indexed: BTreeSet<&str> = scans
        .iter()
        .flat_map(|s| s.indexed.iter().copied())
        .collect();
    let lost: BTreeSet<&str> = scans.iter().flat_map(|s| s.lost.iter().copied()).collect();
    let divisors: BTreeSet<&str> = (scans.iter())
        .flat_map(|s| s.divisors.iter().copied())
        .collect();
    let beside: BTreeSet<(&str, &str)> = (scans.iter())
        .flat_map(|s| s.beside.iter().copied())
        .collect();
    let long: BTreeSet<(&str, i64)> = (scans.iter())
        .flat_map(|s| s.lengths.iter().map(|(f, c)| (f.as_str(), *c)))
        .collect();
    let fields_of = |fs: &[Field], keep: &dyn Fn(&Type) -> bool| -> Vec<String> {
        (fs.iter())
            .filter(|f| keep(&f.ty) && !lost.contains(f.name.as_str()))
            .map(|f| f.name.clone())
            .collect()
    };
    let elements = || scans.iter().map(|s| &s.elements);
    let through: BTreeSet<(&str, &str, i64)> =
        (elements().flat_map(|e| e.through.iter().copied())).collect();
    let loose: BTreeSet<&str> = elements().flat_map(|e| e.loose.iter().copied()).collect();
    let shrunk: BTreeSet<&str> = elements().flat_map(|e| e.shrunk.iter().copied()).collect();
    let array = |t: &Type| matches!(resolve(t), Type::Array(_) | Type::SmallArray(..));
    let mut of: BTreeSet<RecordFact> = BTreeSet::new();
    for (name, d) in decls {
        let Type::Record(fs) = &d.base else { continue };
        if host.contains(name) {
            continue;
        }
        let arrays = fields_of(fs, &array);
        for (i, a) in arrays.iter().enumerate() {
            for b in &arrays[i + 1..] {
                if indexed.contains(a.as_str()) || indexed.contains(b.as_str()) {
                    let (la, lb) = (Part::Len(a.clone()), Part::Len(b.clone()));
                    of.insert(record_fact(vec![(la.clone(), 1), (lb.clone(), -1)]));
                    of.insert(record_fact(vec![(la, -1), (lb, 1)]));
                }
            }
        }
        let seqs = fields_of(fs, &|t| kind_of(t, decls) == Kind::Seq);
        for a in &seqs {
            for (_, c) in long.range((a.as_str(), i64::MIN)..=(a.as_str(), i64::MAX)) {
                of.insert(vec![(Part::Len(a.clone()), 1), (Part::One, -c)]);
            }
        }
        for f in fields_of(fs, &|t| kind_of(t, decls).is_int()) {
            if divisors.contains(f.as_str()) {
                of.insert(vec![(Part::Val(f.clone()), 1), (Part::One, -1)]);
            }
            let near = (seqs.iter()).filter(|a| beside.contains(&(f.as_str(), a.as_str())));
            for a in near {
                of.insert(vec![(Part::Val(f.clone()), 1)]);
                let parts = vec![(Part::Val(f.clone()), -1), (Part::Len(a.clone()), 1)];
                of.insert(record_fact(parts.clone()));
                of.insert(record_fact([parts, vec![(Part::One, -1)]].concat()));
            }
        }
        of.extend(element_facts(fs, decls, &through, &loose, &shrunk));
    }
    for name in host {
        let ty = Type::Named(name.clone());
        if let Some(fs) = vyrn_frontend::types::record_fields(&ty, decls) {
            of.retain(|f| !binds(f, &fs, decls));
        }
    }
    let mut facts = RecordFacts::new(of, decls, &records.params);
    let maybe: Vec<usize> = (0..bodies.len()).filter(|&i| scans[i].builds).collect();
    let touches = in_parallel(
        &maybe,
        |&i| bodies[i].1.names.len(),
        || (),
        |(), &i| built(bodies[i].1, decls, &facts).contains(&true),
    );
    let touching: Vec<usize> = (maybe.iter().zip(touches))
        .filter_map(|(i, t)| t.then_some(*i))
        .collect();
    let mut assumed = vec![BTreeSet::new(); bodies.len()];
    for &i in &touching {
        assumed[i] = parameter_facts(bodies[i], decls, &facts.of, closed);
        if let Some(k) = keep {
            assumed[i].retain(|f| k[i].contains(f));
        }
    }
    // The walks read each body's assumed facts as its `pre`.
    let with_pre: Vec<Summary>;
    let lengths = match assumed.iter().all(BTreeSet::is_empty) {
        true => lengths,
        false => {
            with_pre = (bodies.iter().zip(&assumed).enumerate())
                .map(|(i, ((_, b), a))| {
                    let bare = || Summary {
                        params: (b.params.iter())
                            .map(|p| kind_of(&b.names[p.index()].ty, decls))
                            .collect(),
                        types: Vec::new(),
                        pre: BTreeSet::new(),
                        post: BTreeSet::new(),
                        when: BTreeSet::new(),
                    };
                    Summary {
                        pre: a.clone(),
                        ..lengths.values.get(i).cloned().unwrap_or_else(bare)
                    }
                })
                .collect();
            View {
                values: &with_pre,
                ..lengths
            }
        }
    };
    let weight = |i: &usize| bodies[touching[*i]].1.names.len();
    let mut used: Vec<BTreeSet<RecordFact>> = vec![BTreeSet::new(); touching.len()];
    let mut walked = vec![false; touching.len()];
    let mut queue: Vec<usize> = (0..touching.len()).collect();
    while !facts.is_empty() && !queue.is_empty() {
        queue.sort_by_key(weight);
        let batch: Vec<usize> = queue.drain(..(queue.len() * 2).div_ceil(3)).collect();
        let outs = in_parallel(
            &batch,
            weight,
            || (),
            |(), &i| obliged(bodies[touching[i]].1, decls, &facts, lengths),
        );
        let mut dropped = BTreeSet::new();
        for (&i, (broken, u)) in batch.iter().zip(outs) {
            dropped.extend(broken);
            used[i] = u;
            walked[i] = true;
        }
        if dropped.is_empty() {
            continue;
        }
        let mut of = std::mem::take(&mut facts.of);
        of.retain(|f| !dropped.contains(f));
        facts = RecordFacts::new(of, decls, &records.params);
        for i in 0..touching.len() {
            if !queue.contains(&i) && used[i].iter().any(|f| dropped.contains(f)) {
                queue.push(i);
            }
        }
        queue.retain(|&i| built(bodies[touching[i]].1, decls, &facts).contains(&true));
    }
    // A body no batch walked assumed nothing.
    for (&i, w) in touching.iter().zip(walked) {
        if !w || facts.is_empty() {
            assumed[i].clear();
        }
    }
    (facts, assumed)
}

/// The facts over the parameters of `b`, a body of `closed`, in a
/// [`Summary`]'s terms, that its walk in [`invariants`] assumes at entry:
/// for each array parameter a record literal takes whole as the part of a
/// field, each fact of `of` that names that field alone, over the parameter.
/// A fact on the field's elements is a template of the parameter.
fn parameter_facts(
    (f, b): (FnId, &Body),
    decls: &HashMap<String, TypeDecl>,
    of: &BTreeSet<RecordFact>,
    closed: &HashSet<FnId>,
) -> BTreeSet<Lin> {
    let mut out = BTreeSet::new();
    if !closed.contains(&f) {
        return out;
    }
    for (s, _) in rows(&b.stmts) {
        let St::Let(_, Rhs::Make(Ctor::Record(_, fields), parts)) = s else {
            continue;
        };
        for (field, v) in fields.iter().zip(parts) {
            let Val::Name(m) = v else { continue };
            let Some(k) = b.params.iter().position(|p| p == m) else {
                continue;
            };
            if kind_of(&b.names[m.index()].ty, decls) != Kind::Seq {
                continue;
            }
            let p = Name(k as u32 + 1);
            let term = |part: &Part| match part {
                Part::Elem(x) if x == field => Some(Lin::of(Term::Val(p))),
                Part::Len(x) if x == field => Some(Lin::of(Term::Len(p))),
                Part::One => Some(Lin::k(1)),
                _ => None,
            };
            for fact in of {
                out.extend(
                    fact.iter()
                        .try_fold(Lin::k(0), |l, (x, c)| l.add(&term(x)?.scale(*c)?)),
                );
            }
        }
    }
    out
}

/// What [`invariants`] reads of one body.
struct Scan<'b> {
    /// The fields a check indexes, directly or through a name read from one.
    indexed: Vec<&'b str>,
    /// Each field read of a record name, with each field of the same name a
    /// check indexes.
    beside: Vec<(&'b str, &'b str)>,
    /// The fields a row writes through a place the walk does not track.
    lost: Vec<&'b str>,
    /// The integer fields read into a name a check divides by.
    divisors: Vec<&'b str>,
    /// Each field a check indexes with a constant bound, with the length the
    /// check needs ([`needed_lengths`]).
    lengths: Vec<(String, i64)>,
    /// The callees of the direct calls whose result a record literal holds.
    fills: Vec<FnId>,
    /// Whether a row builds a record or writes a field.
    builds: bool,
    /// What the body does to elements and lengths ([`element_scan`]).
    elements: ElementScan<'b>,
}

fn scan<'b>(b: &'b Body, decls: &HashMap<String, TypeDecl>, needs: &[(Place, i64)]) -> Scan<'b> {
    let mut checked: BTreeSet<Name> = BTreeSet::new();
    let mut divided: BTreeSet<Name> = BTreeSet::new();
    let mut indexed: Vec<&str> = Vec::new();
    let mut lost: Vec<&str> = Vec::new();
    let mut reads: Vec<(Name, Option<Name>, &str)> = Vec::new();
    // Per record name, the fields a check indexes.
    let mut on: Vec<(Name, &str)> = Vec::new();
    let record = |p: &Place| match p {
        Place::Name(r) => Some(*r),
        _ => None,
    };
    let mut builds = false;
    let (mut parts, mut calls): (Vec<Name>, Vec<(Name, FnId)>) = (Vec::new(), Vec::new());
    for (s, _) in rows(&b.stmts) {
        match s {
            St::Check(c) => match &c.guard {
                Guard::Index(Place::Field(r, f), _) | Guard::Span(Place::Field(r, f), ..) => {
                    indexed.push(f);
                    on.extend(record(r).map(|r| (r, f.as_str())));
                }
                Guard::Index(Place::Name(n), _) | Guard::Span(Place::Name(n), ..) => {
                    checked.insert(*n);
                }
                Guard::NonZero(Val::Name(d)) => {
                    divided.insert(*d);
                }
                _ => {}
            },
            St::Let(n, Rhs::Read(Place::Field(r, f))) => reads.push((*n, record(r), f)),
            St::Let(_, Rhs::Make(Ctor::Record(..), vs)) => {
                builds = true;
                parts.extend(vs.iter().filter_map(val_name));
            }
            St::Let(n, rhs) => calls.extend(direct_call(rhs).map(|g| (*n, g))),
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
    for (n, r, f) in &reads {
        if checked.contains(n) {
            indexed.push(f);
            on.extend(r.map(|r| (r, *f)));
        }
    }
    let divisors = (reads.iter())
        .filter(|(n, ..)| divided.contains(n))
        .map(|(.., f)| *f)
        .collect();
    let lengths = (needs.iter())
        .filter_map(|(p, c)| match p {
            Place::Field(r, f) if matches!(**r, Place::Name(_)) => Some((f.clone(), *c)),
            Place::Name(n) => (reads.iter())
                .find(|(m, ..)| m == n)
                .map(|(.., f)| (f.to_string(), *c)),
            _ => None,
        })
        .collect();
    let beside = (reads.iter())
        .flat_map(|(_, r, f)| {
            (on.iter())
                .filter(|(q, _)| Some(*q) == *r)
                .map(|(_, a)| (*f, *a))
        })
        .collect();
    let fills = (calls.into_iter())
        .filter(|(n, _)| parts.contains(n))
        .map(|(_, g)| g)
        .collect();
    let elements = element_scan(b, decls);
    Scan {
        indexed,
        beside,
        lost,
        divisors,
        lengths,
        fills,
        builds: builds || elements.stores,
        elements,
    }
}

/// What one body does to the elements of record fields and to the lengths
/// of fields, for [`element_facts`].
#[derive(Default)]
struct ElementScan<'b> {
    /// Each `(f, g, c)` where a check indexes field `g` of a record name by an
    /// element read from field `f` of the same name, or by such an element
    /// plus a value below `c` ([`reach`]); `c` is 1 for the element alone.
    through: Vec<(&'b str, &'b str, i64)>,
    /// The fields whose elements a row may change other than by a store into
    /// an element or a push the walk holds to the facts
    /// ([`Walk::stored_element`]).
    loose: Vec<&'b str>,
    /// The fields a row may shorten.
    shrunk: Vec<&'b str>,
    /// Whether a row stores into an element of a record name's field.
    stores: bool,
}

/// What `b` does to elements and lengths ([`ElementScan`]). A record
/// literal is the walk's to check ([`Walk::built_elements`]), and so is a
/// lambda, which is a body of its own.
fn element_scan<'b>(b: &'b Body, decls: &HashMap<String, TypeDecl>) -> ElementScan<'b> {
    let mut out = ElementScan::default();
    let mut defs: Vec<Option<&Rhs>> = vec![None; b.names.len()];
    let mut uses: Vec<u32> = vec![0; b.names.len()];
    for (s, _) in rows(&b.stmts) {
        if let St::Let(n, rhs) = s {
            defs[n.index()] = Some(rhs);
        }
        for n in row_writes(s) {
            uses[n.index()] += 1;
        }
    }
    let tracked = |p: &Place| match p {
        Place::Name(r) => {
            vyrn_frontend::types::record_fields(&b.names[r.index()].ty, decls).is_some()
        }
        _ => false,
    };
    for (s, _) in rows(&b.stmts) {
        match s {
            St::Check(c) => through(b, decls, &defs, &uses, &c.guard, &mut out.through),
            St::Store {
                place: Place::Elem(base, _),
                ..
            } => {
                if let Place::Field(r, f) = &**base {
                    match tracked(r) {
                        true => out.stores = true,
                        false => out.loose.push(f),
                    }
                }
            }
            St::Store {
                place: place @ Place::Field(r, x),
                value,
                ..
            } => {
                let (elements, length) = refill(&defs, &uses, place, value, tracked(r));
                if !elements {
                    out.loose.push(x);
                }
                if !length {
                    out.shrunk.push(x);
                }
            }
            _ => {}
        }
        let (St::Let(_, rhs) | St::Do { rhs, .. }) = s else {
            continue;
        };
        let Rhs::Call {
            callee, args, kind, ..
        } = rhs
        else {
            continue;
        };
        // The checker passes a field or an element `modify` only to a
        // builtin (`@pop`, `@swapRemove`); a row it may admit later that
        // moves elements in place loses the field's facts.
        let builtin = matches!(kind, Callee::Builtin | Callee::Reserved);
        let len = match builtin {
            true => length_rule(callee, args),
            false => Length::Unknown,
        };
        for (a, _) in args.iter().filter(|(_, c)| *c != Capability::Read) {
            match a {
                Arg::Place(Place::Field(_, x)) => {
                    if !grows(len) {
                        out.shrunk.push(x);
                    }
                    if !(builtin && keeps_elements(callee, len)) {
                        out.loose.push(x);
                    }
                }
                Arg::Place(Place::Elem(base, _)) => {
                    if let Place::Field(_, f) = &**base {
                        out.loose.push(f);
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// Adds to `out` each `(f, g, c)` of [`ElementScan::through`] the check `guard`
/// gives: it indexes field `g` of a record name, or a borrow of the field
/// ([`field_borrows`]), by an index whose base takes an element of field `f`
/// of the same name, in its `let` or in a row that stores the element's name
/// into it (a loop's state), plus a value below `c` ([`reach`]).
fn through<'b>(
    b: &'b Body,
    decls: &HashMap<String, TypeDecl>,
    defs: &[Option<&'b Rhs>],
    uses: &[u32],
    guard: &'b Guard,
    out: &mut Vec<(&'b str, &'b str, i64)>,
) {
    let Guard::Index(place, Val::Name(i)) = guard else {
        return;
    };
    let Place::Field(r, g) = through_borrow(place, defs, uses) else {
        return;
    };
    if !matches!(**r, Place::Name(_)) {
        return;
    }
    let (v, c) = (defs[i.index()])
        .and_then(|d| reach(b, decls, d, &|x| defs[x.index()]))
        .unwrap_or((*i, 1));
    // The field of `r` whose element the `let` of `n` reads.
    let element = |n: Name| match defs[n.index()] {
        Some(Rhs::Read(Place::Elem(base, _))) => match through_borrow(base, defs, uses) {
            Place::Field(r2, f) if r2 == r => Some(f.as_str()),
            _ => None,
        },
        _ => None,
    };
    out.extend(element(v).map(|f| (f, g.as_str(), c)));
    for (s, _) in rows(&b.stmts) {
        if let St::Store {
            place: Place::Name(n),
            value: Val::Name(m),
            ..
        } = s
        {
            if *n == v {
                out.extend(element(*m).map(|f| (f, g.as_str(), c)));
            }
        }
    }
}

/// Whether a store of `value` into the field `place` keeps the field's
/// elements to the facts, and whether it keeps the field's length from
/// falling: `value` is the field's own array, taken or read, rebuilt only by
/// builtins. The elements stay when each builtin is a `@push` into a tracked
/// record, which the walk holds to the facts ([`Walk::stored_element`]), or
/// keeps the elements it has; an empty array keeps the elements but may
/// shorten the field. Every name of the chain is defined once and written by
/// no other row.
fn refill(
    defs: &[Option<&Rhs>],
    uses: &[u32],
    place: &Place,
    value: &Val,
    tracked: bool,
) -> (bool, bool) {
    let Val::Name(mut cur) = value else {
        return (false, false);
    };
    let (mut elements, mut length) = (true, true);
    // Each step follows a definition, so a chain has at most `defs.len()`.
    for _ in 0..=defs.len() {
        if uses[cur.index()] > 1 {
            return (false, false);
        }
        match defs[cur.index()] {
            Some(Rhs::Take(p) | Rhs::Read(p)) if p == place => return (elements, length),
            Some(Rhs::Make(Ctor::Array, parts)) if parts.is_empty() => return (elements, false),
            Some(Rhs::Call {
                callee,
                args,
                kind: Callee::Builtin | Callee::Reserved,
                ..
            }) => {
                let Some((
                    Arg::Val(Val::Name(t)) | Arg::Place(Place::Name(t)),
                    Capability::Consume,
                )) = args.first()
                else {
                    return (false, false);
                };
                let len = length_rule(callee, args);
                length &= grows(len);
                elements &= (callee == "@push" && tracked) || keeps_elements(callee, len);
                cur = *t;
            }
            _ => return (false, false),
        }
    }
    (false, false)
}

/// The names row `s` writes: the root of each place it stores into or takes
/// from, and of each argument a call takes `modify` or `consume`.
fn row_writes(s: &St) -> impl Iterator<Item = Name> + '_ {
    let args = match s {
        St::Let(_, Rhs::Call { args, .. })
        | St::Do {
            rhs: Rhs::Call { args, .. },
            ..
        } => &args[..],
        _ => &[],
    };
    // `resized_places` holds each such argument that is a place.
    let written = args.iter().filter_map(|(a, c)| match (a, c) {
        (_, Capability::Read) => None,
        (Arg::Val(Val::Name(n)), _) => Some(*n),
        _ => None,
    });
    resized_places(s).filter_map(place_root).chain(written)
}

/// The checks that index by an element of an array name, for the element
/// templates the walk keeps per array ([`State::elems`]): per check whose
/// index takes an element of integer array name `x`, through [`reach`] or a
/// row that stores the element into the index, `x`, the place the check
/// indexes, and the reach `c`. A borrow of an array name (`let b = read x`)
/// is the array. A record field's elements are [`RecordFacts`]' ([`through`]).
fn element_seeds(
    b: &Body,
    decls: &HashMap<String, TypeDecl>,
    ss: &[St],
) -> Vec<(Name, Place, i64)> {
    let mut out = Vec::new();
    let read = |s: &St| match s {
        St::Let(_, Rhs::Read(Place::Elem(a, _))) => match **a {
            Place::Name(a) => int_elements(&b.names[a.index()].ty, decls),
            _ => false,
        },
        _ => false,
    };
    let by_name =
        |s: &St| matches!(s, St::Check(c) if matches!(c.guard, Guard::Index(_, Val::Name(_))));
    if !rows(ss).any(|(s, _)| read(s)) || !rows(ss).any(|(s, _)| by_name(s)) {
        return out;
    }
    // Per name, its `let` and the count of other rows that write it.
    let mut defs: Vec<(Option<&Rhs>, u32)> = vec![(None, 0); b.names.len()];
    for (s, _) in rows(ss) {
        if let St::Let(n, rhs) = s {
            defs[n.index()].0 = Some(rhs);
        }
        for n in row_writes(s) {
            defs[n.index()].1 += 1;
        }
    }
    let root = |n: Name| match defs[n.index()] {
        (Some(Rhs::Read(Place::Name(a))), 0) => *a,
        _ => n,
    };
    let field = |n: Name| match defs[n.index()] {
        (Some(Rhs::Read(q @ Place::Field(r, _))), 0) => matches!(**r, Place::Name(_)).then_some(q),
        _ => None,
    };
    let element = |n: Name| match defs[n.index()].0 {
        Some(Rhs::Read(Place::Elem(base, _))) => match &**base {
            Place::Name(a) if field(*a).is_none() => {
                Some(root(*a)).filter(|x| int_elements(&b.names[x.index()].ty, decls))
            }
            _ => None,
        },
        _ => None,
    };
    for (s, _) in rows(ss) {
        let St::Check(Check {
            guard: Guard::Index(place, Val::Name(i)),
            ..
        }) = s
        else {
            continue;
        };
        let (v, c) = (defs[i.index()].0)
            .and_then(|d| reach(b, decls, d, &|x| defs[x.index()].0))
            .unwrap_or((*i, 1));
        let stored = rows(ss).filter_map(|(s, _)| match s {
            St::Store {
                place: Place::Name(n),
                value: Val::Name(m),
                ..
            } if *n == v => Some(*m),
            _ => None,
        });
        for x in element(v).into_iter().chain(stored.filter_map(element)) {
            let target = match place {
                Place::Name(y) => field(*y).cloned().unwrap_or(Place::Name(root(*y))),
                p => p.clone(),
            };
            out.push((x, target, c));
        }
    }
    out
}

/// `t` with array `from`'s element and length renamed to array `to`'s.
fn renamed(t: &Lin, from: Name, to: Name) -> Option<Lin> {
    t.map(|x| {
        Some(Lin::of(match x {
            Term::Val(n) if n == from => Term::Val(to),
            Term::Len(n) if n == from => Term::Len(to),
            x => x,
        }))
    })
}

/// The template `t` of array `a` for the element `e`.
fn filled(t: &Lin, a: Name, e: &Lin) -> Option<Lin> {
    t.map(|x| {
        Some(if x == Term::Val(a) {
            e.clone()
        } else {
            Lin::of(x)
        })
    })
}

/// Per name a `let` binds to a read of a record name's field `keep` admits
/// and no row writes, that field: the borrow the core hoists above a loop
/// (`let @borrow = read r.f`). An element read or an index through the name
/// reads the field.
fn field_borrows<'s>(ss: &'s [St], keep: impl Fn(&str) -> bool) -> BTreeMap<Name, &'s Place> {
    let mut out = BTreeMap::new();
    for (s, _) in rows(ss) {
        if let St::Let(n, Rhs::Read(p @ Place::Field(r, f))) = s {
            if matches!(**r, Place::Name(_)) && keep(f) {
                out.insert(*n, p);
            }
        }
    }
    if !out.is_empty() {
        for (s, _) in rows(ss) {
            row_writes(s).for_each(|n| _ = out.remove(&n));
        }
    }
    out
}

/// `p`, or the field `p` names when it is a borrow of one ([`field_borrows`]),
/// by each name's `let` and count of writes.
fn through_borrow<'p>(p: &'p Place, defs: &[Option<&'p Rhs>], uses: &[u32]) -> &'p Place {
    match p {
        Place::Name(b) if uses[b.index()] == 0 => match defs[b.index()] {
            Some(Rhs::Read(q @ Place::Field(r, _))) if matches!(**r, Place::Name(_)) => q,
            _ => p,
        },
        _ => p,
    }
}

/// Whether a length effect never shortens.
fn grows(len: Length) -> bool {
    matches!(
        len,
        Length::Keeps | Length::GrowsByOne | Length::GrowsByLenOf(_)
    )
}

/// Whether builtin `callee` with length effect `len` leaves its receiver
/// only elements it held.
fn keeps_elements(callee: &str, len: Length) -> bool {
    let kept = prelude::builtin(callee).is_some_and(|r| {
        matches!(
            r.elements,
            Elements::KeepsEachPosition | Elements::KeepsRange
        )
    });
    kept && matches!(
        len,
        Length::Keeps | Length::ShrinksByOneIfNotEmpty | Length::SetToZero
    )
}

/// Whether `ty` is an array of integers that may index another array: wider
/// than a byte, whose type bounds it, and no `UInt64`. Only such an array
/// carries element templates across a call.
fn index_elements(ty: &Type, decls: &HashMap<String, TypeDecl>) -> bool {
    let t = vyrn_frontend::types::resolved(ty, decls);
    t.elem().is_some_and(
        |e| matches!(kind_of(e, decls), k @ Kind::Int(bits, _) if bits > 8 && k.is_int()),
    )
}

/// Whether `ty` is an array of integers a term can stand for: no `UInt64`.
fn int_elements(ty: &Type, decls: &HashMap<String, TypeDecl>) -> bool {
    let t = vyrn_frontend::types::resolved(ty, decls);
    t.elem().is_some_and(|e| kind_of(e, decls).is_int())
}

/// The element facts of a record type with fields `fs`, Houdini's first
/// guess: for each `(f, g, c)` of `through`, every element of the integer
/// array field `f` is at least zero and at least `c` below the length of
/// field `g`, or `c - 1` below it, since an element may end a half-open
/// range (`e < r.offsets[v + 1]` before `r.targets[e]`). A field of `loose`
/// has no element fact and one of `shrunk` bounds none: a row may change the
/// one's elements or shorten the other where no walk sees it.
fn element_facts(
    fs: &[Field],
    decls: &HashMap<String, TypeDecl>,
    through: &BTreeSet<(&str, &str, i64)>,
    loose: &BTreeSet<&str>,
    shrunk: &BTreeSet<&str>,
) -> Vec<RecordFact> {
    let has = |x: &str, k: &dyn Fn(&Type) -> bool| fs.iter().any(|f| f.name == x && k(&f.ty));
    let mut out = Vec::new();
    for &(f, g, c) in through {
        if loose.contains(f)
            || shrunk.contains(g)
            || !has(f, &|t| int_elements(t, decls))
            || !has(g, &|t| kind_of(t, decls) == Kind::Seq)
        {
            continue;
        }
        let (e, len) = (Part::Elem(f.to_string()), Part::Len(g.to_string()));
        out.push(vec![(e.clone(), 1)]);
        for below in (c - 1).max(0)..=c {
            let parts = vec![(e.clone(), -1), (len.clone(), 1), (Part::One, -below)];
            out.push(record_fact(parts));
        }
    }
    out
}

/// The lengths the checks of `b` need a constant lower bound on, each with
/// the place or name whose length it bounds: one past an index that is an
/// integer literal, a mask `x & m` by a literal, or an unsigned integer
/// narrower than 64 bits converted; and the literal a clause bounds a
/// length below by. None is above `LENGTH_LIMIT`, which no length passes.
fn needed_lengths(b: &Body, decls: &HashMap<String, TypeDecl>) -> Vec<(Place, i64)> {
    let lit = |v: &Val| match v {
        Val::Lit(Lit::Int(i)) => Some(*i),
        Val::Lit(Lit::Byte(i)) => Some(i64::from(*i)),
        _ => None,
    };
    let mut out = Vec::new();
    // The checks that index by a name, read against the name's `let` below.
    let mut by_name: Vec<(&Place, Name)> = Vec::new();
    for (s, _) in rows(&b.stmts) {
        let St::Check(c) = s else { continue };
        match &c.guard {
            Guard::Index(p, Val::Name(i)) => by_name.push((p, *i)),
            Guard::Index(p, i) => out.extend(lit(i).map(|t| (p.clone(), t.saturating_add(1)))),
            Guard::Clause { atoms, .. } => {
                for a in atoms {
                    // `n.length >= c` or `n.length > c`, either way round.
                    let (len, k, strict) = match (&a.l.part, &a.r.part, a.op) {
                        (Some(_), None, BinOp::GtEq) => (&a.l, &a.r, false),
                        (Some(_), None, BinOp::Gt) => (&a.l, &a.r, true),
                        (None, Some(_), BinOp::LtEq) => (&a.r, &a.l, false),
                        (None, Some(_), BinOp::Lt) => (&a.r, &a.l, true),
                        _ => continue,
                    };
                    let (Val::Name(n), Some("length" | "byteLength")) =
                        (&len.of, len.part.as_deref())
                    else {
                        continue;
                    };
                    let c = lit(&k.of).map(|c| c.saturating_add(i64::from(strict)));
                    out.extend(c.map(|c| (Place::Name(*n), c)));
                }
            }
            _ => {}
        }
    }
    if !by_name.is_empty() {
        let mut defs: Vec<Option<&Rhs>> = vec![None; b.names.len()];
        for (s, _) in rows(&b.stmts) {
            if let St::Let(n, rhs) = s {
                defs[n.index()] = Some(rhs);
            }
        }
        for (p, i) in by_name {
            let Some(rhs) = defs[i.index()] else { continue };
            // An index `v + x` needs the reach of `x` where `v` is zero.
            let c = (top(b, decls, rhs).map(|t| t.saturating_add(1)))
                .or_else(|| reach(b, decls, rhs, &|x| defs[x.index()]).map(|(_, c)| c));
            out.extend(c.map(|c| (p.clone(), c)));
        }
    }
    let limit = vyrn_frontend::trap::LENGTH_LIMIT as i64;
    out.retain(|(_, c)| (1..=limit).contains(c));
    out
}

/// The largest value of a name `rhs` defines, read from `rhs` alone: a mask
/// `x & m` by a literal `m`, or an unsigned integer narrower than 64 bits
/// converted.
fn top(b: &Body, decls: &HashMap<String, TypeDecl>, rhs: &Rhs) -> Option<i64> {
    match rhs {
        Rhs::Prim(Op::Bin(BinOp::BitAnd), vs, _) => vs.iter().find_map(|v| match v {
            Val::Lit(Lit::Int(i)) => Some(*i),
            Val::Lit(Lit::Byte(i)) => Some(i64::from(*i)),
            _ => None,
        }),
        Rhs::Prim(Op::Conv(_), vs, _) => match vs.as_slice() {
            [Val::Name(x)] => match kind_of(&b.names[x.index()].ty, decls) {
                Kind::Int(bits, false) if bits < 64 => Some((1i64 << bits) - 1),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

/// The base `v` and the reach `c` of an index `rhs` defines as `v + x`, where
/// the `let` of name `x` (`def`) has a [`top`] below `c`: a check of the
/// index needs `v + c` at most the length.
fn reach<'r>(
    b: &Body,
    decls: &HashMap<String, TypeDecl>,
    rhs: &Rhs,
    def: &dyn Fn(Name) -> Option<&'r Rhs>,
) -> Option<(Name, i64)> {
    let Rhs::Prim(Op::Bin(BinOp::Add), vs, _) = rhs else {
        return None;
    };
    let [Val::Name(p), Val::Name(q)] = vs.as_slice() else {
        return None;
    };
    let reach = |x: &Name| def(*x).and_then(|d| top(b, decls, d)?.checked_add(1));
    (reach(q).map(|c| (*p, c))).or_else(|| reach(p).map(|c| (*q, c)))
}

/// The lengths `b` can give an array it returns: the count of the array
/// literal it returns, the byte length of a String literal a builtin that
/// keeps its operand's length takes (`bytes("..")`), and the bound of a loop
/// ([`bound_test`]) that stores to the returned array, which may push once a
/// turn, or indexes it by the loop's counter.
fn made_lengths(b: &Body) -> BTreeSet<Lin> {
    let returned: BTreeSet<Name> = (rows(&b.stmts))
        .filter_map(|(s, _)| match s {
            St::Return {
                value: Some(Val::Name(n)),
                ..
            } => Some(*n),
            _ => None,
        })
        .collect();
    let mut out = BTreeSet::new();
    for (s, _) in rows(&b.stmts) {
        match s {
            St::Let(n, Rhs::Make(Ctor::Array, parts)) if returned.contains(n) => {
                out.insert(Lin::k(parts.len() as i64));
            }
            St::Let(
                n,
                Rhs::Call {
                    callee,
                    args,
                    kind: Callee::Builtin | Callee::Reserved,
                    ..
                },
            ) if returned.contains(n) => {
                let keeps = prelude::builtin(callee)
                    .is_some_and(|b| b.length_at(args.len()) == Length::Keeps);
                if let (true, Some((Arg::Val(Val::Lit(Lit::Str(x))), _))) = (keeps, args.first()) {
                    out.insert(Lin::k(x.len() as i64));
                }
            }
            St::Loop { body, .. } => {
                let Some((_, k, bound)) = bound_test(body) else {
                    continue;
                };
                let mut stored = BTreeSet::new();
                stores(body, &mut stored);
                let mut indexed = Vec::new();
                seqs(body, &mut indexed);
                let by_k = (indexed.iter()).filter_map(|(p, i)| (*i == k).then(|| place_root(p)));
                stored.extend(by_k.flatten());
                if !stored.is_disjoint(&returned) {
                    out.insert(bound);
                }
            }
            _ => {}
        }
    }
    out
}

/// Per name of `body`, whether it is a record a literal builds with a fact,
/// a literal's integer part a field's constant bound names, or a record
/// whose field a fact names a row writes.
fn built(body: &Body, decls: &HashMap<String, TypeDecl>, facts: &RecordFacts) -> Vec<bool> {
    let mut out = vec![false; body.names.len()];
    for (s, _) in rows(&body.stmts) {
        match s {
            St::Let(n, Rhs::Make(Ctor::Record(_, fields), parts)) => {
                let ty = &body.names[n.index()].ty;
                let of = facts.of_type(ty, decls);
                out[n.index()] |= !of.is_empty();
                for (f, v) in fields.iter().zip(parts) {
                    let named = of
                        .iter()
                        .any(|x| matches!(&x[..], [(Part::Val(g), 1)] | [(Part::Val(g), 1), (Part::One, _)] if g == f));
                    if let (true, Val::Name(m)) = (named, v) {
                        out[m.index()] = true;
                    }
                }
            }
            _ => {
                for p in resized_places(s) {
                    let Place::Field(r, f) = p else { continue };
                    let Place::Name(r) = &**r else { continue };
                    let ty = &body.names[r.index()].ty;
                    out[r.index()] |= facts.of_type(ty, decls).iter().any(|x| mentions(x, f));
                }
            }
        }
    }
    element_writers(body, decls, facts, &mut out);
    out
}

/// Marks in `out` each record name whose field a row stores into, an
/// element or the whole array, when a fact of its type bounds the field's
/// elements, and then each value stored into an element or pushed: the walk
/// holds them to the facts ([`Walk::stored_element`]).
fn element_writers(
    body: &Body,
    decls: &HashMap<String, TypeDecl>,
    facts: &RecordFacts,
    out: &mut [bool],
) {
    let bounds = |r: Name, f: &str| {
        let ty = &body.names[r.index()].ty;
        let e = Part::Elem(f.to_string());
        (facts.of_type(ty, decls).iter()).any(|x| x.iter().any(|(p, _)| *p == e))
    };
    let mut any = false;
    for (s, _) in rows(&body.stmts) {
        let St::Store { place, value, .. } = s else {
            continue;
        };
        let (field, element) = match place {
            Place::Elem(b, _) => (&**b, true),
            p => (p, false),
        };
        let Place::Field(r, f) = field else { continue };
        let Place::Name(r) = **r else { continue };
        if bounds(r, f) {
            any = true;
            out[r.index()] = true;
            if let (true, Val::Name(v)) = (element, value) {
                out[v.index()] = true;
            }
        }
    }
    if !any {
        return;
    }
    for (s, _) in rows(&body.stmts) {
        let (St::Let(_, Rhs::Call { callee, args, .. })
        | St::Do {
            rhs: Rhs::Call { callee, args, .. },
            ..
        }) = s
        else {
            continue;
        };
        if callee == "@push" {
            if let Some(v) = args.get(1).and_then(|(a, _)| root(a)) {
                out[v.index()] = true;
            }
        }
    }
}

/// Whether a part of `fact` is field `f`.
fn mentions(fact: &RecordFact, f: &str) -> bool {
    (fact.iter()).any(|(p, _)| matches!(p, Part::Val(x) | Part::Len(x) if x == f))
}

/// The places row `s` may write a field of or change the length of: a
/// store's, a take's, and each `modify` or `consume` argument's.
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

/// The facts of `facts` that `body` does not prove of a record it builds or
/// writes, where the record's value leaves the name ([`Walk::keeps`]), and
/// the facts the walk assumed ([`Walk::restate`]). The walk starts with the
/// `pre` `lengths` states of the body.
fn obliged(
    body: &Body,
    decls: &HashMap<String, TypeDecl>,
    facts: &RecordFacts,
    lengths: View<'_>,
) -> (BTreeSet<RecordFact>, BTreeSet<RecordFact>) {
    let view = View { facts, ..lengths };
    let none = BTreeSet::new();
    let pre = (body.id.and_then(|f| view.get(f))).map_or(&none, |(_, s)| &s.pre);
    let mut stmts = body.stmts.clone();
    let mut w = Walk::new(body, decls, view, &stmts, Some(None), false);
    w.obliging = true;
    w.seed(&stmts, built(body, decls, facts));
    let st = w.entry(pre);
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
        &|r| sums.linked(r),
        &|_, _| false,
        &BTreeSet::new(),
    );
    (rows(&b.stmts))
        .filter_map(|(s, _)| match s {
            St::Let(n, rhs) if rel[n.index()] => {
                let (i, sum) = sums.get(sums.call(rhs)?)?;
                (!sum.post.is_empty() || !sum.when.is_empty()).then_some(i)
            }
            _ => None,
        })
        .collect()
}

/// The callee's row of a direct call whose result a summary may state: no
/// `fn`-typed argument, so the arguments are the parameters in order. For a
/// generic callee the row names no body; [`View::call`] finds the instance.
fn direct_call(rhs: &Rhs) -> Option<FnId> {
    match rhs {
        Rhs::Call {
            kind: Callee::Fn(g),
            targets,
            ..
        } if targets.is_empty() => Some(*g),
        _ => None,
    }
}

/// `body`'s summary with every candidate of its `post` and no `pre`. The
/// first return of a name or an integer decides the result: an integer's
/// value, an `Option` of an integer's payload, the integer fields of an
/// `Option` of a record's payload, or an array's length; any other result,
/// a String's or a record's included, states nothing. The parameters are
/// those the body never writes. An `UInt64` is left out: the facts read its
/// values above `i64::MAX` as negatives. An integer result equals `c` when
/// every return returns the literal `c` ([`constant_result`]). An array
/// result is at least as long as each array or String parameter, at most as
/// long as each `consume` one, at least `c` long for each `c` of `needed`
/// ([`needed_lengths`]) the body can make ([`made_lengths`]), and at least
/// `p` long for each integer parameter `p` it can make. A payload's integer
/// field is at least zero, equal to or bounded by each integer parameter,
/// and at most each length or below it. A Bool result takes the candidates
/// of [`truth_templates`].
fn templates(
    body: &Body,
    decls: &HashMap<String, TypeDecl>,
    needed: &BTreeSet<i64>,
    pairs: &BTreeSet<Pair>,
) -> Summary {
    let params: Vec<Kind> = (body.params.iter())
        .map(|p| kind_of(&body.names[p.index()].ty, decls))
        .collect();
    let mut fields = Vec::new();
    let r = rows(&body.stmts).find_map(|(s, _)| match s {
        St::Return {
            value: Some(Val::Name(n)),
            ..
        } => {
            let ty = &body.names[n.index()].ty;
            fields = payload_fields(ty, decls);
            Some(match kind_of(ty, decls) {
                _ if *ty == Type::Bool => None,
                k if k.is_int() => Some(Term::Val(Name(0))),
                _ if int_payload(ty, decls) => Some(Term::Val(Name(0))),
                _ if !fields.is_empty() => Some(Term::Field(Name(0), fields[0])),
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
    let truth = truth_result(body);
    let record = returned_type(body).and_then(|ty| match ty {
        Type::Named(d) if pairs.iter().any(|(t, _)| t == d) => Some((ty, d)),
        _ => None,
    });
    let (mut post, mut types, mut written) = (BTreeSet::new(), Vec::new(), BTreeSet::new());
    if truth || matches!(r, Some(Some(_))) || record.is_some() {
        writes(&body.stmts, &mut written);
    }
    let usable = |k: usize| !written.contains(&body.params[k]);
    let sentinel = matches!(r, Some(Some(Term::Val(_)))) && negative_return(body);
    if truth || sentinel || record.is_some() {
        types = (body.params.iter())
            .map(|p| body.names[p.index()].ty.clone())
            .collect();
    }
    if truth {
        post = truth_templates(body, decls, &params, usable);
    }
    let when = match sentinel {
        true => guard_templates(body, decls, usable),
        false => BTreeSet::new(),
    };
    if let Some(Some(r)) = r {
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
            if let Some(c) = constant_result(body) {
                post.extend(r.plus(-c));
                post.extend(Lin::k(c).sub(&r));
            }
        } else if let Term::Field(..) = r {
            for f in fields.into_iter().map(|i| Lin::of(Term::Field(Name(0), i))) {
                post.insert(f.clone());
                for a in &seqs {
                    post.extend(a.sub(&f));
                    post.extend(a.sub(&f).and_then(|l| l.plus(-1)));
                }
                for p in &ints {
                    post.extend(f.sub(p));
                    post.extend(p.sub(&f));
                }
            }
        } else {
            let r = Lin::of(r);
            post.extend(seqs.iter().filter_map(|a| r.sub(a)));
            // At most as long as a `consume` parameter: the only kind
            // `Walk::same_length` reads.
            let owned = |k: usize| !body.names[body.params[k].index()].borrow;
            let consumed = interface(
                &params,
                |k, kind| usable(k) && kind == Kind::Seq && owned(k),
                Term::Len,
            );
            post.extend(consumed.iter().filter_map(|a| a.sub(&r)));
            // Element templates of an index array result: its element,
            // `Val(Name(0))`, at least zero and below a length or an integer
            // parameter ([`State::elems`]).
            if returns_index_array(body, decls) {
                let e = Lin::of(Term::Val(Name(0)));
                post.insert(e.clone());
                post.extend(r.sub(&e).and_then(|l| l.plus(-1)));
                for a in seqs.iter().chain(&ints) {
                    post.extend(a.sub(&e).and_then(|l| l.plus(-1)));
                }
            }
            // A bound the body makes: a needed constant, or an integer
            // parameter in `ints`.
            let param = |n: Name| {
                (body.params.iter().position(|p| *p == n))
                    .map(|k| Lin::of(Term::Val(Name(k as u32 + 1))))
            };
            for m in made_lengths(body) {
                let c = match m.terms[..] {
                    [] if needed.contains(&m.c) => Some(Lin::k(m.c)),
                    [(Term::Val(p), 1)] => param(p)
                        .filter(|p| ints.contains(p))
                        .and_then(|p| p.plus(m.c)),
                    _ => None,
                };
                post.extend(c.and_then(|c| r.sub(&c)));
            }
        }
    }
    // A record of a type that [`Pair`]s with a parameter's: each array field
    // as long as each array of each such parameter the body never writes.
    if let Some((ty, d)) = record {
        let res = array_lengths(ty, Name(0), decls);
        let ps: Vec<Lin> = (0..params.len())
            .filter(|&k| usable(k))
            .filter(|&k| {
                matches!(&body.names[body.params[k].index()].ty,
                    Type::Named(u) if pairs.contains(&(d.clone(), u.clone())))
            })
            .flat_map(|k| {
                array_lengths(
                    &body.names[body.params[k].index()].ty,
                    Name(k as u32 + 1),
                    decls,
                )
            })
            .collect();
        for (a, x) in res.iter().flat_map(|a| ps.iter().map(move |x| (a, x))) {
            post.extend(a.sub(x));
            post.extend(x.sub(a));
        }
    }
    Summary {
        params,
        types,
        pre: BTreeSet::new(),
        post,
        when,
    }
}

/// Two declared record types with array fields, by name, whose lengths a
/// fact may relate: a body indexes a field of the first by a value of the
/// second ([`indexed_pairs`]), and a body makes one from the other
/// ([`made_pairs`]). A fact over two values pays only where both hold; on
/// 2026-10-13 a record paired with an array parameter cost the site
/// export's `elide: summaries` 10% more allocations and proved no row.
type Pair = (String, String);

/// The [`Pair`]s of each record parameter whose array field a check of `b`
/// indexes, with each parameter of another record type with an array field.
fn indexed_pairs(b: &Body, decls: &HashMap<String, TypeDecl>) -> Vec<Pair> {
    let mut out = Vec::new();
    for (k, _) in indexed_fields(b, decls, &b.stmts) {
        let p = b.params[k];
        if let Type::Named(t) = &b.names[p.index()].ty {
            out.extend(holders(b, decls, p, t).map(|u| (t.clone(), u)));
        }
    }
    out
}

/// The [`Pair`]s of the record type `b` returns with each parameter of
/// another record type with an array field.
fn made_pairs(b: &Body, decls: &HashMap<String, TypeDecl>) -> Vec<Pair> {
    match returned_type(b) {
        Some(ty @ Type::Named(t)) if kind_of(ty, decls) == Kind::Other => {
            holders(b, decls, Name(u32::MAX), t)
                .map(|u| (t.clone(), u))
                .collect()
        }
        _ => Vec::new(),
    }
}

/// The type of the name the first return of `b` returns.
fn returned_type(b: &Body) -> Option<&Type> {
    rows(&b.stmts).find_map(|(s, _)| match s {
        St::Return {
            value: Some(Val::Name(n)),
            ..
        } => Some(&b.names[n.index()].ty),
        _ => None,
    })
}

/// The record types other than `t` of the parameters of `b` other than
/// `skip` that have an array field.
fn holders<'b>(
    b: &'b Body,
    decls: &'b HashMap<String, TypeDecl>,
    skip: Name,
    t: &'b str,
) -> impl Iterator<Item = String> + 'b {
    (b.params.iter().filter(move |q| **q != skip))
        .map(|q| &b.names[q.index()].ty)
        .filter(|ty| !array_lengths(ty, Name(0), decls).is_empty())
        .filter_map(move |ty| match ty {
            Type::Named(u)
                if u != t && vyrn_frontend::types::record_fields(ty, decls).is_some() =>
            {
                Some(u.clone())
            }
            _ => None,
        })
}

/// The array fields of record parameters a kept check of `ss` indexes,
/// directly or through a `let` that borrows the field: each as the
/// parameter's position and the field's name.
fn indexed_fields(b: &Body, decls: &HashMap<String, TypeDecl>, ss: &[St]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let record = |p: &Name| kind_of(&b.names[p.index()].ty, decls) == Kind::Other;
    if !b.params.iter().any(record) {
        return out;
    }
    // Per name, its `let` and the count of rows that write it.
    let mut defs: Vec<(Option<&Rhs>, u32)> = vec![(None, 0); b.names.len()];
    for (s, _) in rows(ss) {
        if let St::Let(n, rhs) = s {
            defs[n.index()].0 = Some(rhs);
        }
        for n in row_writes(s) {
            defs[n.index()].1 += 1;
        }
    }
    for (s, _) in rows(ss) {
        let St::Check(c) = s else { continue };
        let Guard::Index(base, _) = &c.guard else {
            continue;
        };
        let base = match base {
            Place::Name(x) => match defs[x.index()] {
                (Some(Rhs::Read(q)), 0) => q,
                _ => continue,
            },
            _ => base,
        };
        let Place::Field(r, f) = base else { continue };
        let Place::Name(r) = **r else { continue };
        let Some(k) = b.params.iter().position(|p| *p == r) else {
            continue;
        };
        if c.verdict != Verdict::Kept {
            continue;
        }
        let fields = vyrn_frontend::types::record_fields(&b.names[r.index()].ty, decls);
        let array = |x: &Field| vyrn_frontend::types::resolved(&x.ty, decls).is_seq();
        if fields.is_some_and(|fs| fs.iter().any(|x| x.name == *f && array(x))) {
            out.push((k, f.clone()));
        }
    }
    out.sort();
    out.dedup();
    out
}

/// The lengths of a value of type `ty` named `n`, as terms: its own for an
/// array, one per class of equal length ([`col`]) of its array fields for a
/// record, none for a String or anything else.
fn array_lengths(ty: &Type, n: Name, decls: &HashMap<String, TypeDecl>) -> Vec<Lin> {
    let array = |t: &Type| vyrn_frontend::types::resolved(t, decls).is_seq();
    if array(ty) {
        return vec![Lin::of(Term::Len(n))];
    }
    let Some(fields) = vyrn_frontend::types::record_fields(ty, decls) else {
        return Vec::new();
    };
    let mut out: Vec<Lin> = (fields.iter().filter(|f| array(&f.ty)))
        .filter_map(|f| Some(Lin::of(Term::Col(n, col(ty, &f.name, decls)?.1))))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The candidates of a sentinel's `when`: for each check row that indexes a
/// parameter's array or array field by a parameter's integer or integer
/// field, `i >= 0` and `len(a) - i - 1 >= 0` over the interface. The guards
/// a sentinel reads under are the facts its callers want.
fn guard_templates(
    body: &Body,
    decls: &HashMap<String, TypeDecl>,
    usable: impl Fn(usize) -> bool,
) -> BTreeSet<Lin> {
    let param = |n: Name| {
        let k = body
            .params
            .iter()
            .position(|p| *p == n)
            .filter(|&k| usable(k))?;
        Some((k, Name(k as u32 + 1)))
    };
    let field = |r: Name, f: &str| {
        let (k, p) = param(r)?;
        let fields =
            vyrn_frontend::types::record_fields(&body.names[body.params[k].index()].ty, decls)?;
        let at = fields.iter().position(|x| x.name == f)?;
        Some((p, u32::try_from(at).ok()?, kind_of(&fields[at].ty, decls)))
    };
    let int = |v: &Val| -> Option<Lin> {
        let Val::Name(n) = v else { return None };
        if let Some((_, p)) =
            param(*n).filter(|_| kind_of(&body.names[n.index()].ty, decls).is_int())
        {
            return Some(Lin::of(Term::Val(p)));
        }
        if writes_of(&body.stmts, *n) != 1 {
            return None;
        }
        let Some(Rhs::Read(Place::Field(r, f))) = let_of(&body.stmts, *n) else {
            return None;
        };
        let Place::Name(r) = **r else { return None };
        let (p, i, kind) = field(r, f)?;
        kind.is_int().then_some(Lin::of(Term::Field(p, i)))
    };
    let seq = |pl: &Place| -> Option<Lin> {
        match pl {
            Place::Name(n) => {
                let (_, p) = param(*n)?;
                (kind_of(&body.names[n.index()].ty, decls) == Kind::Seq)
                    .then_some(Lin::of(Term::Len(p)))
            }
            Place::Field(r, f) => {
                let Place::Name(r) = **r else { return None };
                let (p, i, kind) = field(r, f)?;
                (kind == Kind::Seq).then_some(Lin::of(Term::Col(p, i)))
            }
            _ => None,
        }
    };
    let mut out = BTreeSet::new();
    for (s, _) in rows(&body.stmts) {
        let St::Check(c) = s else { continue };
        let Guard::Index(pl, i) = &c.guard else {
            continue;
        };
        if let (Some(a), Some(i)) = (seq(pl), int(i)) {
            out.extend(a.sub(&i).and_then(|l| l.plus(-1)));
            out.insert(i);
        }
    }
    out
}

/// Whether some return of `body` returns a negative integer literal, or a
/// name bound once to `0 - k` with `k` a positive literal.
fn negative_return(body: &Body) -> bool {
    let negative = |n: Name| {
        writes_of(&body.stmts, n) == 1
            && matches!(let_of(&body.stmts, n), Some(Rhs::Prim(Op::Bin(BinOp::Sub), vs, _))
                if matches!(vs.as_slice(), [Val::Lit(Lit::Int(0)), Val::Lit(Lit::Int(k))] if *k > 0))
    };
    rows(&body.stmts).any(|(s, _)| match s {
        St::Return { value: Some(v), .. } => match v {
            Val::Lit(Lit::Int(c)) => *c < 0,
            Val::Name(n) => negative(*n),
            Val::Lit(_) => false,
        },
        _ => false,
    })
}

/// Whether the first return of `body` returns a Bool.
fn truth_result(body: &Body) -> bool {
    rows(&body.stmts)
        .find_map(|(s, _)| match s {
            St::Return { value: Some(v), .. } => Some(match v {
                Val::Lit(l) => matches!(l, Lit::Bool(_)),
                Val::Name(n) => body.names[n.index()].ty == Type::Bool,
            }),
            _ => None,
        })
        .unwrap_or(false)
}

/// The candidates of a Bool result's `post`: an index in range, `p >= 0` and
/// `len(a) - p - 1 >= 0`, over the integers `p` and lengths `a` of the
/// parameters `usable` keeps and of their records' fields, one deep. None
/// without a length: a predicate over integers alone would link every
/// caller's arguments for facts their types mostly state already.
fn truth_templates(
    body: &Body,
    decls: &HashMap<String, TypeDecl>,
    params: &[Kind],
    usable: impl Fn(usize) -> bool,
) -> BTreeSet<Lin> {
    let (mut ints, mut seqs) = (Vec::new(), Vec::new());
    for (k, kind) in params.iter().enumerate().filter(|(k, _)| usable(*k)) {
        let p = Name(k as u32 + 1);
        match kind {
            k if k.is_int() => ints.push(Lin::of(Term::Val(p))),
            Kind::Seq => seqs.push(Lin::of(Term::Len(p))),
            _ => {
                let ty = &body.names[body.params[k].index()].ty;
                let fields = vyrn_frontend::types::record_fields(ty, decls).unwrap_or_default();
                for (i, f) in (0..).zip(fields.iter()) {
                    match kind_of(&f.ty, decls) {
                        k if k.is_int() => ints.push(Lin::of(Term::Field(p, i))),
                        Kind::Seq => seqs.push(Lin::of(Term::Col(p, i))),
                        _ => {}
                    }
                }
            }
        }
    }
    if seqs.is_empty() {
        return BTreeSet::new();
    }
    let mut out: BTreeSet<Lin> = ints.iter().cloned().collect();
    for a in &seqs {
        out.extend(ints.iter().filter_map(|p| a.sub(p)?.plus(-1)));
    }
    out
}

/// The integer literal every return of `body` returns, if there is one.
fn constant_result(body: &Body) -> Option<i64> {
    let mut values = rows(&body.stmts).filter_map(|(s, _)| match s {
        St::Return { value, .. } => Some(match value {
            Some(Val::Lit(Lit::Int(c))) => Some(*c),
            _ => None,
        }),
        _ => None,
    });
    let first = values.next()??;
    values.all(|v| v == Some(first)).then_some(first)
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
/// [`templates`]. The facts the record facts' walk `assumed` at entry are
/// candidates too.
fn entry_templates(
    body: &Body,
    decls: &HashMap<String, TypeDecl>,
    sums: View<'_>,
    pairs: &BTreeSet<Pair>,
    assumed: &BTreeSet<Lin>,
) -> (BTreeSet<Lin>, Option<Body>) {
    let params: Vec<Kind> = (body.params.iter())
        .map(|p| kind_of(&body.names[p.index()].ty, decls))
        .collect();
    let candidates = |ss: &[St]| {
        let rel = relevant(
            body,
            decls,
            ss,
            false,
            true,
            &|r| sums.linked(r),
            &|_, _| false,
            &BTreeSet::new(),
        );
        let used = |k: usize| rel[body.params[k].index()];
        let ints = interface(&params, |k, kind| used(k) && kind.is_int(), Term::Val);
        let seqs = interface(&params, |k, kind| used(k) && kind == Kind::Seq, Term::Len);
        let mut out = assumed.clone();
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
        // An array field of a record parameter a kept check indexes is at
        // least as long as each array of another parameter its type
        // [`Pair`]s with.
        for (k, f) in indexed_fields(body, decls, ss) {
            let ty = &body.names[body.params[k].index()].ty;
            let (Some((_, i)), Type::Named(t)) = (col(ty, &f, decls), ty) else {
                continue;
            };
            let a = Lin::of(Term::Col(Name(k as u32 + 1), i));
            for j in (0..params.len()).filter(|&j| j != k && used(j)) {
                let ty = &body.names[body.params[j].index()].ty;
                if matches!(ty, Type::Named(u) if pairs.contains(&(t.clone(), u.clone()))) {
                    let xs = array_lengths(ty, Name(j as u32 + 1), decls);
                    out.extend(xs.iter().filter_map(|x| a.sub(x)));
                }
            }
        }
        // Element templates of an index array parameter whose element
        // indexes a kept check ([`element_seeds`]): its element,
        // `Val(Name(k + 1))`, at least zero and below a length or an integer
        // parameter, the indexed parameter's length and its own included
        // ([`State::elems`]).
        let indexed = |k: &usize| index_elements(&body.names[body.params[*k].index()].ty, decls);
        if !(0..params.len()).any(|k| indexed(&k)) {
            return out;
        }
        let param = |n: &Name| body.params.iter().position(|p| p == n);
        let seeds = element_seeds(body, decls, ss);
        let mut bounds = seqs.clone();
        for (_, target, _) in &seeds {
            if let Some(j) = (place_root(target).filter(|_| matches!(target, Place::Name(_))))
                .and_then(|t| param(&t))
            {
                bounds.push(Lin::of(Term::Len(Name(j as u32 + 1))));
            }
        }
        bounds.extend(ints.iter().cloned());
        // So does a parameter whose element the body passes to a call.
        let passed = elements_passed(body, decls, ss);
        let rows: BTreeSet<usize> = (seeds.iter().filter_map(|(x, ..)| param(x)))
            .chain((0..params.len()).filter(|&k| passed.contains(&body.params[k])))
            .filter(indexed)
            .collect();
        for k in rows {
            let e = Lin::of(Term::Val(Name(k as u32 + 1)));
            out.insert(e.clone());
            // A permutation's elements lie below its own length.
            let own = Lin::of(Term::Len(Name(k as u32 + 1)));
            for a in bounds.iter().chain([&own]) {
                out.extend(a.sub(&e));
                out.extend(a.sub(&e).and_then(|l| l.plus(-1)));
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

/// A body's `post` and `when` facts ([`Summary`]).
type Kept = (BTreeSet<Lin>, BTreeSet<Lin>);

/// The facts of `cands`' `post` and `when` every live return of `body`
/// proves, walking it with the callees' facts `sums` states, and the
/// summaries it took facts from, by index. A path that ends without a value
/// proves none.
fn returns(
    body: &Body,
    decls: &HashMap<String, TypeDecl>,
    sums: View<'_>,
    cands: &Summary,
) -> (Kept, BTreeSet<usize>) {
    let mut stmts = body.stmts.clone();
    let mut w = Walk::new(
        body,
        decls,
        sums,
        &stmts,
        Some(Some(cands.post.clone())),
        false,
    );
    w.when = Some(cands.when.clone()).filter(|c| !c.is_empty());
    let st = w.entry(&BTreeSet::new());
    let end = w.block(st, &mut stmts);
    match (w.post, w.when) {
        (Some(kept), when) if end.dead => ((kept, when.unwrap_or_default()), w.read),
        _ => ((BTreeSet::new(), BTreeSet::new()), w.read),
    }
}

/// Per callee of `body` with a `pre`, by index: the facts of it every live
/// call row of `body` proves, walking it with the callees' facts `sums`
/// states and `pre` at its own entry.
fn entries(
    body: &Body,
    decls: &HashMap<String, TypeDecl>,
    sums: View<'_>,
    pre: &BTreeSet<Lin>,
) -> BTreeMap<usize, BTreeSet<Lin>> {
    let mut stmts = body.stmts.clone();
    let mut w = Walk::new(body, decls, sums, &stmts, Some(None), false);
    let st = w.entry(pre);
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
    /// While solving: the same of the body's `when` ([`Summary::when`]).
    when: Option<BTreeSet<Lin>>,
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
    /// The record names whose fields the walk tracks from a literal or a row
    /// that wrote a field, rather than assuming their [`RecordFacts`].
    dirty: BTreeSet<Name>,
    /// While obliging: the facts a record of `dirty` did not prove where its
    /// value left the name.
    broken: BTreeSet<RecordFact>,
    /// While obliging: the facts [`Walk::restate`] assumed.
    assumed: BTreeSet<RecordFact>,
    /// While obliging: per array name taken or read from a record name's
    /// field, or rebuilt from one, that record name and field
    /// ([`Walk::element_write`]).
    pushed: BTreeMap<Name, (Name, String)>,
    /// Per global, the names a `let` binds to its whole value and no other
    /// row writes ([`global_borrows`]).
    borrows: BTreeMap<String, Vec<Name>>,
    /// The borrows of record fields whose elements a [`RecordFacts`] fact
    /// bounds ([`field_borrows`]).
    fields: BTreeMap<Name, Place>,
    /// The element templates a check may need ([`element_seeds`]), each with
    /// the array whose element and length it names. An array starts with
    /// those its elements meet ([`Walk::element_flow`]).
    elem_cands: Vec<(Name, Lin)>,
    /// Per borrow of an array name the walk keeps templates of, the array.
    arrays: BTreeMap<Name, Name>,
    /// In a walk for [`element_rows`]: per parameter, whether every return
    /// so far holds its `result` row and its `own` row ([`Frame`]).
    rows: Option<(Vec<bool>, Vec<bool>)>,
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

/// Whether `ty` is an `Option` of an integer whose values are linear terms.
fn int_payload(ty: &Type, decls: &HashMap<String, TypeDecl>) -> bool {
    let ty = vyrn_frontend::types::resolved(ty, decls);
    vyrn_frontend::types::option_payload(&ty).is_some_and(|p| kind_of(p, decls).is_int())
}

/// The integer fields, by index, of the record an `Option` of a record `ty`
/// holds: the payload's [`Term::Field`]s. Empty for any other type.
fn payload_fields(ty: &Type, decls: &HashMap<String, TypeDecl>) -> Vec<u32> {
    let ty = vyrn_frontend::types::resolved(ty, decls);
    let fields = vyrn_frontend::types::option_payload(&ty)
        .and_then(|p| vyrn_frontend::types::record_fields(p, decls));
    (0..)
        .zip(fields.iter().flat_map(|fs| fs.iter()))
        .filter(|(_, f)| kind_of(&f.ty, decls).is_int())
        .map(|(i, _)| i)
        .collect()
}

/// The name a `Some(v)` row wraps.
fn some_part(rhs: &Rhs) -> Option<Name> {
    match rhs {
        Rhs::Call {
            callee,
            args,
            kind: Callee::Ctor,
            ..
        } if callee == "Some" => match args.as_slice() {
            [(Arg::Val(Val::Name(v)), _)] => Some(*v),
            _ => None,
        },
        _ => None,
    }
}

/// Whether `rhs` makes a `None`.
fn none_row(rhs: &Rhs) -> bool {
    matches!(rhs, Rhs::Call { callee, args, kind: Callee::Ctor, .. } if callee == "None" && args.is_empty())
}

/// The binder of `a` when it is the `Some` arm of a switch on an `Option` of
/// an integer or of a record with an integer field, `on`.
fn some_binder(body: &Body, decls: &HashMap<String, TypeDecl>, on: &Val, a: &Arm) -> Option<Name> {
    let Val::Name(o) = on else { return None };
    let ty = &body.names[o.index()].ty;
    match (&a.test, a.binds.as_slice()) {
        (Test::Tag(1), [b]) if int_payload(ty, decls) || !payload_fields(ty, decls).is_empty() => {
            Some(*b)
        }
        _ => None,
    }
}

impl<'a> Walk<'a> {
    /// A walk that decides `body`'s checks when `solve` is `None`, and
    /// otherwise serves the solve with the `post` candidates it holds. With
    /// `ghosts`, it serves [`element_rows`]: each index array parameter
    /// starts with its elements between its two ghosts.
    fn new(
        body: &'a Body,
        decls: &'a HashMap<String, TypeDecl>,
        sums: impl Into<View<'a>>,
        stmts: &[St],
        solve: Option<Option<BTreeSet<Lin>>>,
        ghosts: bool,
    ) -> Walk<'a> {
        let sums = sums.into();
        let solving = solve.is_some();
        let post = solve.flatten();
        // Argument `k` of a call to a body with a `pre` that names parameter `k`.
        let enters = |g: FnId, k: usize| {
            let named = |f: &Lin| {
                (f.terms.iter()).any(|(t, _)| t.name().is_some_and(|n| n.index() == k + 1))
            };
            solving && sums.get(g).is_some_and(|(_, s)| s.pre.iter().any(named))
        };
        let linked = |r: &Rhs| sums.linked(r);
        // Only a walk that decides checks reads a template: no `pre` or
        // `post` states one.
        let seeds = match solving {
            true => Vec::new(),
            false => element_seeds(body, decls, stmts),
        };
        let mut roots: BTreeSet<Name> = seeds.iter().map(|(x, ..)| *x).collect();
        let own_pre = body.id.and_then(|f| sums.get(f)).map(|(_, s)| &s.pre);
        let post_templates =
            (post.as_ref()).is_some_and(|c| c.iter().any(|l| l.coef(Term::Val(Name(0))) != 0));
        roots.extend(template_roots(
            body,
            decls,
            stmts,
            sums,
            own_pre,
            post_templates,
        ));
        let ghosted: Vec<(usize, Name)> = match ghosts {
            true => (body.params.iter().copied().enumerate())
                .filter(|(_, p)| index_elements(&body.names[p.index()].ty, decls))
                .collect(),
            false => Vec::new(),
        };
        if !ghosted.is_empty() {
            // A ghost walk keeps the templates of the parameters and of each
            // array the body returns: a row reads no other.
            roots.extend(ghosted.iter().map(|(_, p)| *p));
            roots.extend(rows(stmts).filter_map(|(s, _)| match s {
                St::Return {
                    value: Some(Val::Name(r)),
                    ..
                } if index_elements(&body.names[r.index()].ty, decls) => Some(*r),
                _ => None,
            }));
        }
        let mut w = Walk {
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
                &roots,
            )
            .into_iter()
            .map(|relevant| Slot {
                relevant,
                origin: Origin::Local,
            })
            .collect(),
            solving,
            post,
            when: None,
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
            pushed: BTreeMap::new(),
            borrows: global_borrows(stmts),
            fields: (field_borrows(stmts, |f| sums.facts.bounds_elements_of(f)).into_iter())
                .map(|(n, p)| (n, p.clone()))
                .collect(),
            elem_cands: Vec::new(),
            arrays: BTreeMap::new(),
            rows: None,
        };
        for (k, p) in ghosted {
            w.elem_cands
                .extend(ghost_pair(k, p).into_iter().map(|t| (p, t)));
        }
        w.template_cands(stmts, own_pre, post_templates);
        if seeds.is_empty() && w.elem_cands.is_empty() {
            return w;
        }
        for (x, target, c) in &seeds {
            let Some(len) = w.length(target).map(Lin::of) else {
                continue;
            };
            let e = Lin::of(Term::Val(*x));
            w.elem_cands.push((*x, e.clone()));
            for below in (c - 1).max(0)..=*c {
                if let Some(t) = len.sub(&e).and_then(|l| l.plus(-below)) {
                    w.elem_cands.push((*x, t));
                }
            }
        }
        w.elem_cands.sort();
        w.elem_cands.dedup();
        for (s, _) in rows(stmts) {
            if let St::Let(b, Rhs::Read(Place::Name(a))) = s {
                if roots.contains(a) {
                    w.arrays.insert(*b, *a);
                }
            }
        }
        for (s, _) in rows(stmts) {
            row_writes(s).for_each(|n| _ = w.arrays.remove(&n));
        }
        w
    }

    /// Adds to [`Walk::elem_cands`] the templates a summary states: the
    /// body's own `pre` of each array parameter, its `post` candidates of
    /// each array it returns, and each callee's `pre` of the array argument
    /// at each call row.
    fn template_cands(&mut self, ss: &[St], pre: Option<&BTreeSet<Lin>>, post: bool) {
        let params = &self.body.params;
        let seq = |j: usize| {
            let p = j.checked_sub(1).and_then(|k| params.get(k));
            p.is_some_and(|p| self.kind(*p) == Kind::Seq)
        };
        let mut out: Vec<(Name, Lin)> = Vec::new();
        for f in pre.into_iter().flatten() {
            if let Some(j) = hole(f, seq) {
                let p = params[j.index() - 1];
                out.extend(f.map(|t| self.param_term(t)).map(|l| (p, l)));
            }
        }
        if let (true, Some(cands)) = (post, self.post.as_ref()) {
            let hole = Term::Val(Name(0));
            each_row(ss, &mut |s| {
                let St::Return {
                    value: Some(Val::Name(r)),
                    ..
                } = s
                else {
                    return;
                };
                if !index_elements(&self.body.names[r.index()].ty, self.decls) {
                    return;
                }
                let at = |t: Term| match t {
                    _ if t == hole => Some(Lin::of(Term::Val(*r))),
                    Term::Len(Name(0)) => Some(Lin::of(Term::Len(*r))),
                    _ => self.param_term(t),
                };
                let ts = cands.iter().filter(|c| c.coef(hole) != 0);
                out.extend(ts.filter_map(|c| c.map(at)).map(|l| (*r, l)));
            });
        }
        each_row(ss, &mut |s| {
            let Some((_, sum, args)) = self.sums.pre(s) else {
                return;
            };
            let seq = |j: usize| {
                j.checked_sub(1)
                    .is_some_and(|k| sum.params.get(k) == Some(&Kind::Seq))
            };
            for f in &sum.pre {
                let Some(j) = hole(f, seq) else { continue };
                let Some(a) = args.get(j.index() - 1).and_then(|(a, _)| root(a)) else {
                    continue;
                };
                let at = |t: Term| {
                    if t == Term::Val(j) {
                        return Some(Lin::of(Term::Val(a)));
                    }
                    let k = t.name()?.index().checked_sub(1)?;
                    self.arg(t, &args.get(k)?.0, *sum.params.get(k)?)
                };
                out.extend(f.map(at).map(|l| (a, l)));
            }
        });
        self.elem_cands.extend(out);
    }

    /// The array a place's elements are: an array name, or the array a
    /// borrow of one reads.
    fn array_of(&self, p: &Place) -> Option<Name> {
        match p {
            Place::Name(b) => Some(self.arrays.get(b).copied().unwrap_or(*b)),
            _ => None,
        }
    }

    /// The candidate templates ([`Walk::elem_cands`]) over array `n`.
    fn cands_for(&self, n: Name) -> BTreeSet<Lin> {
        (self.elem_cands.iter())
            .filter_map(|(x, t)| renamed(t, *x, n))
            .collect()
    }

    /// Whether `st` proves template `t` of array `a` of the value `v`, with a
    /// check's three premises: an element read the template proves is one.
    fn meets(&self, st: &State, t: &Lin, a: Name, v: &Val) -> bool {
        let goal = self.lin(v).and_then(|e| filled(t, a, &e));
        goal.is_some_and(|g| (st.prover().ge0_deep(&g)).is_some_and(|cert| cert.verify(st, &g)))
    }

    /// The element templates of `n = rhs`: an array literal starts with each
    /// candidate its parts meet; a builtin that rebuilds its first argument
    /// keeps that argument's templates its length change keeps true, and a
    /// push only those the pushed value meets; a take or copy of an array
    /// name has the name's. A template of another array bounded by the
    /// source's length is bounded by `n`'s where the length moved its way.
    fn element_flow(&self, st: &mut State, n: Name, rhs: &Rhs) {
        if self.elem_cands.is_empty() || self.kind(n) != Kind::Seq {
            return;
        }
        let (m, len) = match rhs {
            Rhs::Make(Ctor::Array, parts) => {
                let ts: BTreeSet<Lin> = (self.cands_for(n).into_iter())
                    .filter(|t| parts.iter().all(|v| self.meets(st, t, n, v)))
                    .collect();
                if !ts.is_empty() {
                    st.elems.insert(n, ts);
                }
                return;
            }
            Rhs::Val(Val::Name(m)) | Rhs::Take(Place::Name(m)) => (*m, Length::Keeps),
            Rhs::Call {
                callee,
                args,
                kind: Callee::Builtin | Callee::Reserved,
                ..
            } if lands_on_result(args) => match args.first().and_then(|(a, _)| root(a)) {
                Some(m) => (m, length_rule(callee, args)),
                None => return,
            },
            // A direct call's result has every template of each argument
            // whose `result` row holds ([`element_rows`]), over the caller's
            // terms before the call, the argument's own length included: the
            // result may be shorter. The call's effects then move them.
            Rhs::Call { args, .. } => {
                let mut ts = BTreeSet::new();
                for (k, (a, _)) in args.iter().enumerate() {
                    let row = self.sums.frame(rhs, k).is_some_and(|f| f.result);
                    if let Some(m) = root(a).filter(|_| row) {
                        let m = self.arrays.get(&m).copied().unwrap_or(m);
                        let theirs = st.elems.get(&m).into_iter().flatten();
                        ts.extend(
                            theirs.filter_map(|t| t.subst(Term::Val(m), &Lin::of(Term::Val(n)))),
                        );
                    }
                }
                if !ts.is_empty() {
                    st.elems.insert(n, ts);
                }
                return;
            }
            _ => return,
        };
        let m = self.arrays.get(&m).copied().unwrap_or(m);
        // Whether a template with `k * len(x)` stays true as `x` becomes `n`.
        let follows = |t: &Lin, x: Name| {
            let k = t.coef(Term::Len(x));
            match len {
                Length::Keeps => true,
                Length::GrowsByOne | Length::GrowsByLenOf(_) => k >= 0,
                Length::ShrinksByOneIfNotEmpty => k <= 0,
                _ => k == 0,
            }
        };
        let bounded: Vec<(Name, Lin)> = (st.elems.iter())
            .filter(|(a, _)| **a != m && **a != n)
            .flat_map(|(a, ts)| ts.iter().map(move |t| (*a, t)))
            .filter(|(_, t)| t.coef(Term::Len(m)) != 0 && follows(t, m))
            .filter_map(|(a, t)| Some((a, renamed(t, m, n)?)))
            .collect();
        for (a, t) in bounded {
            st.elems.entry(a).or_default().insert(t);
        }
        let from = |y: Name, st: &State| -> Vec<Lin> {
            let y = self.arrays.get(&y).copied().unwrap_or(y);
            (st.elems.get(&y).into_iter().flatten())
                .filter_map(|t| renamed(t, y, n))
                .collect()
        };
        let row = match rhs {
            Rhs::Call { callee, .. } => prelude::builtin(callee).map(|r| r.elements),
            _ => Some(Elements::KeepsEachPosition),
        };
        let ts: BTreeSet<Lin> = match (row, len) {
            (Some(_), Length::SetToZero) => self.cands_for(n),
            (Some(Elements::KeepsEachPosition | Elements::KeepsRange), _) => {
                let args: &[(Arg, Capability)] = match rhs {
                    Rhs::Call { args, .. } => args,
                    _ => &[],
                };
                let added = match len {
                    Length::GrowsByOne => args.get(1).map(|(a, _)| a),
                    _ => None,
                };
                let theirs = match len {
                    Length::GrowsByLenOf(j) => args.get(j).and_then(|(a, _)| root(a)),
                    _ => None,
                }
                .map(|y| from(y, st));
                (from(m, st).into_iter())
                    .filter(|t| follows(t, n))
                    .filter(|t| theirs.as_ref().is_none_or(|y| y.contains(t)))
                    .filter(|t| match added {
                        Some(Arg::Val(v)) => self.meets(st, t, n, v),
                        Some(Arg::Place(Place::Name(v))) => self.meets(st, t, n, &Val::Name(*v)),
                        Some(Arg::Place(_)) => false,
                        None => true,
                    })
                    .collect()
            }
            (Some(Elements::OfArg(j)), Length::SetToLenOf(l)) if j == l => {
                let Rhs::Call { args, .. } = rhs else { return };
                match args.get(j).and_then(|(a, _)| root(a)) {
                    Some(y) => from(y, st).into_iter().collect(),
                    None => return,
                }
            }
            _ => return,
        };
        if !ts.is_empty() {
            st.elems.insert(n, ts);
        }
    }

    /// Whether a template of array `a` in `st` implies `want`, a template of
    /// `a` too: the two differ by what `st` proves at least zero, with the
    /// same coefficient on the hole.
    fn implied(&self, st: &State, a: Name, want: &Lin) -> bool {
        let hole = Term::Val(a);
        let k = want.coef(hole);
        let prover = st.prover();
        let theirs =
            (st.elems.get(&a).into_iter().flatten().cloned()).chain(self.field_templates(a));
        (k != 0)
            && (theirs.filter(|t| t.coef(hole) == k))
                .filter_map(|t| want.sub(&t))
                .any(|d| prover.ge0_deep(&d).is_some_and(|cert| cert.verify(st, &d)))
    }

    /// The element facts of the record field the borrow `b` reads, each as
    /// a template of `b` ([`RecordFacts`]).
    fn field_templates(&self, b: Name) -> Vec<Lin> {
        let Some(Place::Field(r, f)) = self.fields.get(&b) else {
            return Vec::new();
        };
        let Place::Name(r) = **r else {
            return Vec::new();
        };
        let e = Lin::of(Term::Val(b));
        (self.element_facts_of(r, Some(f)).into_iter())
            .filter_map(|fact| self.element_lin(r, fact, &e, None))
            .collect()
    }

    /// Keeps of array `a`, whose element `place` takes `value`, the
    /// templates `value` meets.
    fn element_store(&self, st: &mut State, a: Name, value: &Val) {
        let Some(ts) = st.elems.get(&a) else { return };
        let kept: BTreeSet<Lin> = (ts.iter())
            .filter(|t| self.meets(st, t, a, value))
            .cloned()
            .collect();
        match kept.is_empty() {
            true => _ = st.elems.remove(&a),
            false => _ = st.elems.insert(a, kept),
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
            let p = *params.get(t.name()?.index().checked_sub(1)?)?;
            match t {
                Term::Val(_) => Some(Lin::of(Term::Val(p))),
                Term::Len(_) => Some(Lin::of(Term::Len(p))),
                Term::Col(..) | Term::Field(..) => self.param_term(t),
                Term::Inner(..) | Term::Ghost(..) => None,
            }
        };
        let seq = |j: usize| {
            let p = j.checked_sub(1).and_then(|k| params.get(k));
            p.is_some_and(|p| self.kind(*p) == Kind::Seq)
        };
        for f in pre {
            let Some(l) = f.map(at) else { continue };
            match hole(f, seq) {
                Some(j) => {
                    let p = params[j.index() - 1];
                    st.elems.entry(p).or_default().insert(l);
                }
                None => st.assume(&l),
            }
        }
        if self.rows.is_some() {
            for (k, p) in params.iter().enumerate() {
                if index_elements(&self.body.names[p.index()].ty, self.decls) {
                    st.elems.insert(*p, ghost_pair(k, *p).into_iter().collect());
                }
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
    /// field of a record name ([`Walk::field`]).
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

    /// The length of `p`, an element of an array name or of a record name's
    /// array field that an index name picks, when the element is an array
    /// ([`Term::Inner`]).
    fn inner(&self, p: &Place) -> Option<Term> {
        let Place::Elem(base, Val::Name(i)) = p else {
            return None;
        };
        let seq = |ty: &Type| {
            let ty = vyrn_frontend::types::resolved(ty, self.decls);
            ty.elem()
                .is_some_and(|e| kind_of(e, self.decls) == Kind::Seq)
        };
        match &**base {
            Place::Name(r) if seq(&self.body.names[r.index()].ty) => {
                Some(Term::Inner(*r, None, *i))
            }
            Place::Field(r, f) => {
                let Place::Name(r) = **r else { return None };
                let ty = &self.body.names[r.index()].ty;
                let fields = vyrn_frontend::types::record_fields(ty, self.decls)?;
                let at = fields.iter().position(|x| x.name == *f)?;
                let at = u32::try_from(at).ok().filter(|_| seq(&fields[at].ty))?;
                Some(Term::Inner(r, Some(at), *i))
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

    /// The [`RecordFacts`] record name `n`'s type has, each over the walk's
    /// terms of `n`'s fields.
    fn facts_of(&self, n: Name) -> Vec<(Lin, &'a RecordFact)> {
        let facts = self.sums.facts;
        if facts.is_empty() || self.kind(n) != Kind::Other {
            return Vec::new();
        }
        let ty = &self.body.names[n.index()].ty;
        let term = |p: &Part| match p {
            Part::Val(f) => self.field(&Place::Name(n), f).map(Lin::of),
            Part::Len(f) => self.col_term(n, f).map(Lin::of),
            Part::Elem(_) => None,
            Part::One => Some(Lin::k(1)),
        };
        let lin = |fact: &RecordFact| {
            (fact.iter()).try_fold(Lin::k(0), |l, (p, k)| l.add(&term(p)?.scale(*k)?))
        };
        (facts.of_type(ty, self.decls).into_iter())
            .filter_map(|f| Some((lin(f)?, f)))
            .collect()
    }

    /// The facts of record name `r`'s type that bound the elements of field
    /// `f`, or of any field when `f` is `None`.
    fn element_facts_of(&self, r: Name, f: Option<&str>) -> Vec<&'a RecordFact> {
        let facts = self.sums.facts;
        let bounds = |p: &Part| matches!(p, Part::Elem(x) if f.is_none_or(|f| x == f));
        let none = !facts.of.iter().any(|x| x.iter().any(|(p, _)| bounds(p)));
        if none || self.kind(r) != Kind::Other {
            return Vec::new();
        }
        let ty = &self.body.names[r.index()].ty;
        (facts.of_type(ty, self.decls).into_iter())
            .filter(|x| x.iter().any(|(p, _)| bounds(p)))
            .collect()
    }

    /// `fact` over the walk's terms of record name `r`, with `e` for the
    /// element, and the length of field `grown` one more than its term: a
    /// push holds its element to the length it leaves.
    fn element_lin(&self, r: Name, fact: &RecordFact, e: &Lin, grown: Option<&str>) -> Option<Lin> {
        fact.iter().try_fold(Lin::k(0), |l, (p, k)| {
            let t = match p {
                Part::Elem(_) => e.clone(),
                Part::Len(g) => Lin::of(self.col_term(r, g)?).plus(i64::from(grown == Some(g)))?,
                Part::Val(x) => Lin::of(self.field(&Place::Name(r), x)?),
                Part::One => Lin::k(1),
            };
            l.add(&t.scale(*k)?)
        })
    }

    /// Assumes of `n`, an element read from the field `base` or a borrow of
    /// it, the field's element facts, which every write into it proved
    /// ([`invariants`]).
    fn element_read(&mut self, st: &mut State, n: Name, base: &Place) {
        if let Some(a) = self.array_of(base) {
            let e = Lin::of(Term::Val(n));
            let ts: Vec<Lin> = (st.elems.get(&a).into_iter().flatten())
                .filter_map(|t| filled(t, a, &e))
                .collect();
            ts.iter().for_each(|l| st.assume(l));
        }
        let base = match base {
            Place::Name(b) => self.fields.get(b).unwrap_or(base),
            _ => base,
        };
        let Place::Field(r, f) = base else { return };
        let Place::Name(r) = **r else { return };
        let e = Lin::of(Term::Val(n));
        for fact in self.element_facts_of(r, Some(f)) {
            if let Some(l) = self.element_lin(r, fact, &e, None) {
                st.assume(&l);
                if self.obliging {
                    self.assumed.insert(fact.clone());
                }
            }
        }
    }

    /// While obliging, drops each element fact of field `f` of record name
    /// `r` that `v`, stored into an element of the field or pushed onto it
    /// when `push`, does not prove in `st`.
    fn stored_element(&mut self, st: &State, r: Name, f: &str, v: Option<Val>, push: bool) {
        if !self.obliging || !self.record || st.dead {
            return;
        }
        let e = v.and_then(|v| self.lin(&v));
        let holds = |g: &Lin| st.ge0(g).is_some_and(|cert| cert.verify(st, g));
        for fact in self.element_facts_of(r, Some(f)) {
            let proved = (e.as_ref())
                .and_then(|e| self.element_lin(r, fact, e, push.then_some(f)))
                .is_some_and(|g| holds(&g));
            if !proved {
                self.broken.insert(fact.clone());
            }
        }
    }

    /// While obliging, drops each element fact the record literal `n` of
    /// `fields` from `parts` does not keep: the field's part is empty, an
    /// array literal whose every element the fact holds of over the other
    /// parts, an array whose element templates imply the fact, or a copy of
    /// the same field of a record name `src`, and each field the fact bounds
    /// it by is at least as long as `src`'s.
    fn built_elements(&mut self, st: &State, n: Name, fields: &[String], parts: &[Val]) {
        if !self.record || st.dead {
            return;
        }
        let part = |x: &str| match fields.iter().position(|y| y == x).map(|i| &parts[i]) {
            Some(Val::Name(m)) => Some(*m),
            _ => None,
        };
        let holds = |g: &Lin| st.ge0(g).is_some_and(|cert| cert.verify(st, g));
        for fact in self.element_facts_of(n, None) {
            let Some(Part::Elem(f)) = fact
                .iter()
                .map(|(p, _)| p)
                .find(|p| matches!(p, Part::Elem(_)))
            else {
                continue;
            };
            let m = part(f);
            let empty =
                m.is_some_and(|m| holds(&Lin::of(Term::Len(m)).scale(-1).expect("one term")));
            let from = m.and_then(|m| self.pushed.get(&m)).filter(|(_, x)| x == f);
            let longer = |(src, _): &(Name, String)| {
                fact.iter().all(|(p, _)| match p {
                    Part::Len(g) => {
                        let mine = part(g).map(|m| Lin::of(Term::Len(m)));
                        let theirs = self.col_term(*src, g).map(Lin::of);
                        mine.zip(theirs)
                            .and_then(|(a, b)| a.sub(&b))
                            .is_some_and(|d| holds(&d))
                    }
                    _ => true,
                })
            };
            let over = |p: &Part, e: &Lin| match p {
                Part::Elem(_) => Some(e.clone()),
                Part::Len(g) => Some(Lin::of(Term::Len(part(g)?))),
                Part::Val(x) => self.lin(&parts[fields.iter().position(|y| y == x)?]),
                Part::One => Some(Lin::k(1)),
            };
            // The fact over the parts, with `e` for the element.
            let at = |e: &Lin| {
                fact.iter()
                    .try_fold(Lin::k(0), |l, (p, k)| l.add(&over(p, e)?.scale(*k)?))
            };
            let holds_of = |v: &Val| at(&self.lin(v)?);
            // A template the part holds implies the fact of every element.
            let templated = m.is_some_and(|m| {
                at(&Lin::of(Term::Val(m))).is_some_and(|w| self.implied(st, m, &w))
            });
            // The literal is the part's value when its `let` is its one write.
            let unwritten = |m: &Name| writes_of(&self.body.stmts, *m) == 1;
            let listed = match m
                .filter(unwritten)
                .and_then(|m| let_of(&self.body.stmts, m))
            {
                Some(Rhs::Make(Ctor::Array, items)) => {
                    items.iter().all(|v| holds_of(v).is_some_and(|g| holds(&g)))
                }
                _ => false,
            };
            if !empty && !listed && !templated && !from.is_some_and(longer) {
                self.broken.insert(fact.clone());
            }
        }
    }

    /// While obliging, follows the arrays taken or read from a record name's
    /// field through the builtins that rebuild them ([`refill`]), and holds
    /// each `@push` onto one to the field's element facts.
    fn element_write(&mut self, st: &State, n: Name, rhs: &Rhs) {
        if !self.obliging {
            return;
        }
        match rhs {
            Rhs::Take(Place::Field(r, f)) | Rhs::Read(Place::Field(r, f)) => {
                if let Place::Name(r) = **r {
                    self.pushed.insert(n, (r, f.clone()));
                }
            }
            Rhs::Make(Ctor::Record(_, fields), parts) => self.built_elements(st, n, fields, parts),
            Rhs::Call {
                callee,
                args,
                kind: Callee::Builtin | Callee::Reserved,
                ..
            } => {
                let from = match args.first() {
                    // `x.f.copy()`: the copy holds the field's elements.
                    Some((Arg::Place(Place::Field(r, f)), _)) if callee == "@copy" => match **r {
                        Place::Name(r) => Some((r, f.clone())),
                        _ => None,
                    },
                    a => (a.and_then(|(a, _)| root(a)))
                        .and_then(|t| self.pushed.get(&t))
                        .cloned(),
                };
                let Some((r, f)) = from else { return };
                if callee == "@push" {
                    let v = args.get(1).and_then(|(a, _)| match a {
                        Arg::Val(v) => Some(v.clone()),
                        Arg::Place(Place::Name(m)) => Some(Val::Name(*m)),
                        Arg::Place(_) => None,
                    });
                    self.stored_element(st, r, &f, v, true);
                }
                self.pushed.insert(n, (r, f));
            }
            _ => {}
        }
    }

    /// Forgets which field each name `s` writes held the elements of
    /// ([`Walk::element_write`]), and every such field of a record name `s`
    /// replaces or hands a call to write whole: the record's bounding field
    /// may then be another array.
    fn unpush(&mut self, s: &St) {
        if !self.obliging {
            return;
        }
        for n in row_writes(s) {
            self.pushed.remove(&n);
        }
        let whole: Vec<Name> = match s {
            St::Store {
                place: Place::Name(r),
                ..
            } => vec![*r],
            St::Let(_, Rhs::Call { args, .. })
            | St::Do {
                rhs: Rhs::Call { args, .. },
                ..
            } => (args.iter())
                .filter(|(_, c)| *c != Capability::Read)
                .filter_map(|(a, _)| match a {
                    Arg::Place(Place::Name(r)) | Arg::Val(Val::Name(r)) => Some(*r),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        self.pushed.retain(|_, (r, _)| !whole.contains(r));
    }

    /// Assumes the [`RecordFacts`] of record name `n`, whose value a walk of
    /// another row or body proved them of ([`invariants`]).
    fn restate(&mut self, st: &mut State, n: Name) {
        for (l, f) in self.facts_of(n) {
            st.assume(&l);
            if self.obliging {
                self.assumed.insert(f.clone());
            }
        }
    }

    /// While obliging, drops each fact record name `n` of [`Walk::dirty`] does
    /// not prove in `st`, where its value leaves the name for a reader that
    /// assumes the facts: a call, a store, a literal, a return or a release.
    fn keeps(&mut self, st: &State, n: Name) {
        if !self.obliging || !self.record || st.dead || !self.dirty.contains(&n) {
            return;
        }
        let holds = |g: &Lin| st.ge0(g).is_some_and(|cert| cert.verify(st, g));
        for (l, f) in self.facts_of(n) {
            if !holds(&l) {
                self.broken.insert(f.clone());
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
    /// assumes its facts after the call.
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

    /// Notes that a row wrote field `f` of record name `r`, when a fact of
    /// `r`'s type names `f`. A borrow that is no parameter proves its facts
    /// after the row: its source is read through another name, which assumes
    /// them.
    fn wrote_field(&mut self, st: &State, r: Name, f: &str) {
        if !self.facts_of(r).iter().any(|(_, x)| mentions(x, f)) {
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
            mark(
                self.body,
                self.decls,
                ss,
                &mut rel,
                Goals::Obligations,
                &|r| sums.linked(r),
                &BTreeSet::new(),
            );
            if rel.iter().filter(|r| **r).count() == before {
                break;
            }
        }
        for (s, r) in self.slot.iter_mut().zip(rel) {
            s.relevant = r;
        }
    }

    /// [`col`] of record name `r`'s type.
    fn col(&self, r: Name, f: &str) -> Option<(u32, u32)> {
        col(&self.body.names[r.index()].ty, f, self.decls)
    }

    /// The integer field `f` of record name `r` as one term: by the module's
    /// postulate its value changes only where the walk sees `r` written.
    fn field(&self, r: &Place, f: &str) -> Option<Term> {
        let Place::Name(r) = r else { return None };
        let ty = &self.body.names[r.index()].ty;
        let fields = vyrn_frontend::types::record_fields(ty, self.decls)?;
        let at = (fields.iter())
            .position(|x| x.name == f && matches!(kind_of(&x.ty, self.decls), Kind::Int(..)))?;
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

    /// Whether `n` is an `Option` of an integer. Its [`Term::Val`] is the
    /// payload's value: a fact over it holds whenever `n` is `Some`, so a
    /// `None` satisfies every one.
    fn payload(&self, n: Name) -> bool {
        int_payload(&self.body.names[n.index()].ty, self.decls)
    }

    /// [`payload_fields`] of `n`'s type. A fact over `n`'s
    /// `Term::Field(n, i)` holds whenever `n` is `Some`, as over a
    /// [`Walk::payload`].
    fn payload_fields(&self, n: Name) -> Vec<u32> {
        payload_fields(&self.body.names[n.index()].ty, self.decls)
    }

    /// Defines the payload of `n = Some(v)` as `v`, or its integer fields as
    /// `v`'s.
    fn wrapped(&self, st: &mut State, n: Name, rhs: &Rhs) {
        let Some(v) = some_part(rhs) else { return };
        if let Some(l) = self.lin(&Val::Name(v)).filter(|_| self.payload(n)) {
            st.define(Term::Val(n), &l);
        }
        for i in self.payload_fields(n) {
            st.define(Term::Field(n, i), &Lin::of(Term::Field(v, i)));
        }
    }

    /// Defines the binder of the `Some` arm `a` of a switch on `on` as the
    /// payload, so it takes every fact the state holds of it.
    fn unwrapped(&self, st: &mut State, on: &Val, a: &Arm) {
        let (Some(b), Val::Name(o)) = (some_binder(self.body, self.decls, on, a), on) else {
            return;
        };
        if !self.slot[b.index()].relevant {
            return;
        }
        if self.payload(*o) {
            st.define(Term::Val(b), &Lin::of(Term::Val(*o)));
        }
        for i in self.payload_fields(*o) {
            st.define(Term::Field(b, i), &Lin::of(Term::Field(*o, i)));
        }
    }

    /// Whether every write of `m` binds it to `None`: a return of it states
    /// every payload fact.
    fn only_none(&self, m: Name) -> bool {
        if !self.payload(m) && self.payload_fields(m).is_empty() {
            return false;
        }
        let nones = (rows(&self.body.stmts))
            .filter(|(s, _)| matches!(s, St::Let(n, rhs) if *n == m && none_row(rhs)))
            .count();
        nones > 0 && nones == writes_of(&self.body.stmts, m)
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
            let p = st.prover();
            p.proves(&hi) && p.proves(&lo)
        };
        let r = r?;
        let halves = || parts.len() == 2 && parts.iter().all(|p| within(p, EXACT / 2));
        (within(&r, EXACT) || halves()).then_some(r)
    }

    fn block(&mut self, mut st: State, ss: &mut [St]) -> State {
        for s in ss {
            // A summary every return so far refuted gains nothing from the rest.
            if self.post.as_ref().is_some_and(BTreeSet::is_empty) && self.when.is_none() {
                return State::dead();
            }
            if st.dead {
                // A check in code the walk takes for dead is proved: a dead
                // state proves every goal.
                self.dead(s);
                continue;
            }
            let stale = self.stale(s);
            st = self.stmt(st, s);
            self.track(&mut st, s, stale);
            self.unpush(s);
        }
        st
    }

    fn dead(&mut self, s: &mut St) {
        match s {
            St::Check(c) if self.record => c.verdict = Verdict::Proved,
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
            St::Store {
                place,
                value,
                releases,
                ..
            } => {
                if !matches!(place, Place::Name(_)) {
                    st.forget_inner();
                }
                if let Val::Name(m) = value {
                    self.keeps(&st, *m);
                }
                if let Place::Elem(base, _) = &*place {
                    if let Place::Field(r, f) = &**base {
                        if let Place::Name(r) = **r {
                            self.stored_element(&st, r, f, Some(value.clone()), false);
                        }
                    }
                }
                // A store below an element (`xs[i][j] = v`) may change an
                // element no template names; one into an element holds it to
                // each template.
                match (&*place, place_root(place)) {
                    (Place::Elem(base, _), _) if matches!(**base, Place::Name(_)) => {
                        if let Some(a) = self.array_of(base) {
                            self.element_store(&mut st, a, value);
                        }
                    }
                    (Place::Name(_), _) => {}
                    (_, Some(a)) => _ = st.elems.remove(&a),
                    _ => {}
                }
                if let Place::Name(n) = place {
                    // The value the store displaces is released, where the
                    // plan releases one: a value a `consume` took is not.
                    if *releases {
                        self.keeps(&st, *n);
                    }
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
                            self.wrote_field(&st, *r, f);
                        }
                    }
                    // An integer field takes the stored value. A store into
                    // any other field keeps every length.
                    (Place::Field(r, f), None) => {
                        if let (Place::Name(n), Some(t)) = (&**r, self.field(r, f)) {
                            st.forget(t);
                            let v = self.lin(value).filter(|_| self.slot[n.index()].relevant);
                            if let Some(v) = v {
                                st.define(t, &v);
                            }
                            self.wrote_field(&st, *n, f);
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
                        self.element_flow(&mut st, *n, &Rhs::Val(value.clone()));
                        // A record the walk tracks takes another name's value,
                        // whose facts `keeps` held above.
                        if matches!(value, Val::Name(_)) && self.dirty.contains(n) {
                            self.restate(&mut st, *n);
                        }
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
                    Val::Lit(Lit::Bool(true)) => (st, State::dead()),
                    Val::Lit(Lit::Bool(false)) => (State::dead(), st),
                    Val::Name(c) => {
                        let (mut yes, mut no) = (st.clone(), st);
                        // Adding a fact reads no condition; a state it kills has none.
                        if let Some((t, f)) = no.conds.remove(c) {
                            t.iter().for_each(|l| yes.add(l));
                            f.iter().for_each(|l| no.add(l));
                            if !no.dead {
                                no.conds.insert(*c, (t, f));
                            }
                        }
                        (yes, no)
                    }
                    _ => (st.clone(), st),
                };
                let mut a = self.block(yes, then);
                let mut b = self.block(no, els);
                if a.dead || b.dead {
                    return State::join(&mut [a, b]);
                }
                leave(&mut a, then, &[]);
                leave(&mut b, els, &[]);
                let mut ends = [a, b];
                let mut j = State::join(&mut ends);
                self.branch_facts(&mut j, cond, [then, els], &ends);
                j
            }
            St::Block { body, .. } => self.block(st, body),
            St::Loop { body, .. } => self.looped(st, body),
            St::Switch { on, arms, .. } => {
                let mut outs = vec![st];
                for a in arms.iter_mut() {
                    let mut s = outs[0].clone();
                    for b in &a.binds {
                        self.slot[b.index()].origin = self.origin_of_val(on);
                        self.fresh(&mut s, *b);
                    }
                    self.unwrapped(&mut s, on, a);
                    outs.push(self.block(s, &mut a.body));
                }
                if outs.iter().filter(|s| !s.dead).count() > 1 {
                    for (s, a) in outs[1..].iter_mut().zip(arms.iter()) {
                        leave(s, &a.body, &a.binds);
                    }
                }
                State::join(&mut outs)
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
                    // A check's goal takes a chain of three premises; a join's
                    // and a loop's facts take two.
                    let holds =
                        |g: &Lin| (st.prover().ge0_deep(g)).is_some_and(|cert| cert.verify(&st, g));
                    let failing = goals.iter().flatten().find(|g| !holds(g));
                    if goals.is_some() && failing.is_none() {
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
                // A passed index, span, range or shift check states its
                // goals; a divisor's goals are only one way it can pass.
                if matches!(
                    c.guard,
                    Guard::Index(..)
                        | Guard::Span(..)
                        | Guard::Range(..)
                        | Guard::Shift(..)
                        | Guard::Rule(..)
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

    /// Keeps in `j`, the join of `if c`'s branch ends `ends`, each branch's
    /// facts over the names it stores as the side of `c` that branch took:
    /// `let t = if fast { table() } else { [] }` leaves `len(t) >= 256` where
    /// `fast` holds. Only where neither branch writes `c`.
    fn branch_facts(&self, j: &mut State, c: &Val, branches: [&[St]; 2], ends: &[State; 2]) {
        let Val::Name(c) = c else { return };
        if self.solving || !self.slot[c.index()].relevant {
            return;
        }
        let (mut stored, mut written) = (BTreeSet::new(), BTreeSet::new());
        for b in branches {
            stores(b, &mut stored);
            writes(b, &mut written);
        }
        if stored.is_empty() || written.contains(c) {
            return;
        }
        let known = j.prover();
        let [t, f] = ends.each_ref().map(|s| {
            (s.facts.iter().cloned().chain(s.unshared(j)))
                .filter(|l| {
                    (l.terms.iter()).any(|(t, _)| {
                        matches!(t, Term::Len(_) | Term::Col(..))
                            && t.name().is_some_and(|n| stored.contains(&n))
                    })
                })
                .filter(|l| !known.proves(l))
                .map(Fact::Ge)
                .collect::<Vec<_>>()
        });
        if t.is_empty() && f.is_empty() {
            return;
        }
        let side = j.conds.entry(*c).or_default();
        side.0.extend(t);
        side.1.extend(f);
    }

    /// What must be `>= 0` for the check to pass, as far as linear facts can
    /// say; `None` when they cannot say it all.
    fn goals(&self, st: &State, g: &Guard) -> Option<Vec<Lin>> {
        // An element's length bounds nothing the state does not name.
        let len = |p: &Place| {
            (self.length(p).or_else(|| self.borrowed(st, p)))
                .or_else(|| self.inner(p).filter(|t| st.names(*t)))
                .map(Lin::of)
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
                    false if st.differs(&d) => vec![],
                    // Otherwise only a divisor of one sign proves nonzero.
                    false => vec![d.plus(-1)?],
                }
            }
            Guard::NoOverflow(_, d, _) => vec![self.lin(d)?],
            Guard::Range(s, a, b) => {
                let Val::Name(s) = s else { return None };
                let (a, b) = (self.lin(a)?, self.lin(b)?);
                vec![a.clone(), b.sub(&a)?, len(&Place::Name(*s))?.sub(&b)?]
            }
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

    /// The length of `p`, a field `g.f` of a global, as the same field of a
    /// borrow of `g` the state holds [`State::fresh`]: equal to `g`, so its
    /// facts are `g`'s.
    fn borrowed(&self, st: &State, p: &Place) -> Option<Term> {
        let Place::Field(g, f) = p else { return None };
        let Place::Global(g) = &**g else { return None };
        let b = self.borrows.get(g)?.iter().find(|b| st.fresh.contains(b))?;
        self.col_term(*b, f)
    }

    /// Marks after row `s` the borrows `stale` ([`Walk::stale`]) no longer
    /// fresh, and the borrow `s` binds fresh. A stale borrow keeps its own
    /// facts: by the kernel's exclusivity judgment a row that writes its
    /// global ends it.
    fn track(&self, st: &mut State, s: &St, stale: Vec<Name>) {
        for b in &stale {
            st.fresh.remove(b);
        }
        if let St::Let(n, Rhs::Read(Place::Global(g))) = s {
            if !st.dead && self.borrows.get(g).is_some_and(|bs| bs.contains(n)) {
                st.fresh.insert(*n);
            }
        }
    }

    /// The borrows ([`Walk::borrows`]) row `s` may make unequal to their
    /// global: those of each global it stores into, takes from or hands a
    /// call to write, and of each global a direct call may store into by the
    /// effect judgment ([`crate::effects::StateUse`]). A call the judgment
    /// has no frame for or whose frame may call the host, any call but a
    /// direct one or a constructor, and a release may write any global.
    fn stale(&self, s: &St) -> Vec<Name> {
        if self.borrows.is_empty() {
            return Vec::new();
        }
        let every = || self.borrows.values().flatten().copied().collect();
        let mut gs: Vec<&str> = Vec::new();
        match s {
            St::Store { place, .. } | St::Let(_, Rhs::Take(place)) => gs.extend(global_root(place)),
            St::Let(_, rhs @ Rhs::Call { args, kind, .. })
            | St::Do {
                rhs: rhs @ Rhs::Call { args, kind, .. },
                ..
            } => {
                let written = args.iter().filter(|(_, c)| *c != Capability::Read);
                gs.extend(written.filter_map(|(a, _)| match a {
                    Arg::Place(p) => global_root(p),
                    Arg::Val(_) => None,
                }));
                let frame = self.sums.call(rhs).and_then(|f| self.sums.state.get(&f));
                match (kind, frame) {
                    (Callee::Ctor, _) => {}
                    (Callee::Fn(_), Some(u)) if !u.host => {
                        gs.extend(u.writes.iter().map(String::as_str))
                    }
                    _ => return every(),
                }
            }
            St::Drop(..) | St::Row { .. } => return every(),
            _ => {}
        }
        (gs.iter().filter_map(|g| self.borrows.get(*g)))
            .flatten()
            .copied()
            .collect()
    }

    fn bind(&mut self, st: &mut State, n: Name, rhs: &Rhs) {
        let o = self.origin_of(n, rhs);
        self.slot[n.index()].origin = self.slot[n.index()].origin.join(o);
        self.element_write(st, n, rhs);
        st.kill(n);
        // A take leaves a hole the store that fills it may resize.
        if let Rhs::Take(_) = rhs {
            st.forget_inner();
        }
        // A literal's fields are its parts', not the facts'.
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
                if let Some(t) = self.length(b).or_else(|| self.inner(b)) {
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
            Rhs::Read(Place::Elem(base, _)) if self.kind(n).is_int() => {
                self.element_read(st, n, base);
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
                    if let (Some(t), Some(x)) = (self.field(&Place::Name(n), f), self.lin(v)) {
                        st.define(t, &x);
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
            Rhs::Call { .. } if some_part(rhs).is_some() => self.wrapped(st, n, rhs),
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
        self.same_length(st, n, rhs);
        self.range(st, n);
        self.element_flow(st, n, rhs);
        self.effects(st, rhs);
        self.called(st, n, rhs);
    }

    /// Defines the length of `n = rhs`, a direct call, as a `consume`
    /// argument's, where the callee's summary states its result exactly as
    /// long as that parameter. It reads the argument before the call's
    /// effects forget it, so what the state knows of the argument's length
    /// carries over to the result ([`State::kill`]).
    fn same_length(&self, st: &mut State, n: Name, rhs: &Rhs) {
        let Some((_, s)) = self.sums.call(rhs).and_then(|g| self.sums.get(g)) else {
            return;
        };
        let Rhs::Call { args, .. } = rhs else { return };
        if self.kind(n) != Kind::Seq || args.len() != s.params.len() {
            return;
        }
        let r = Lin::of(Term::Len(Name(0)));
        let exact = |p: &Lin| {
            [r.sub(p), p.sub(&r)]
                .iter()
                .flatten()
                .all(|f| s.post.contains(f))
        };
        let len = (0..args.len())
            .filter(|&k| args[k].1 == Capability::Consume && s.params[k] == Kind::Seq)
            .find_map(|k| {
                let p = Term::Len(Name(k as u32 + 1));
                exact(&Lin::of(p)).then(|| self.arg(p, &args[k].0, Kind::Seq))?
            });
        if let Some(l) = len {
            st.define(Term::Len(n), &l);
        }
    }

    /// Assumes what the callee's summary states of `n = rhs`, a direct call,
    /// read after the call's effects. A fact naming an argument the call may
    /// write is left out, and so is one with a term no argument gives.
    fn called(&mut self, st: &mut State, n: Name, rhs: &Rhs) {
        let Some((i, s)) = self.sums.call(rhs).and_then(|g| self.sums.get(g)) else {
            return;
        };
        let Rhs::Call { args, .. } = rhs else { return };
        if (s.post.is_empty() && s.when.is_empty()) || args.len() != s.params.len() {
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
            let Some(k) = t.name()?.index().checked_sub(1) else {
                return match t {
                    Term::Val(_) if self.kind(n).is_int() || self.payload(n) => {
                        Some(Lin::of(Term::Val(n)))
                    }
                    Term::Field(_, i) if self.payload_fields(n).contains(&i) => {
                        Some(Lin::of(Term::Field(n, i)))
                    }
                    Term::Len(_) if self.kind(n) == Kind::Seq => Some(Lin::of(Term::Len(n))),
                    Term::Col(_, i) if self.kind(n) == Kind::Other => {
                        Some(Lin::of(Term::Col(n, i)))
                    }
                    _ => None,
                };
            };
            let a = &args.get(k).filter(|_| copied(k))?.0;
            match t {
                Term::Field(_, i) | Term::Col(_, i) => self.field_arg(t, i, a, s.types.get(k)?),
                _ => self.arg(t, a, s.params[k]),
            }
        };
        let free = |l: &Lin| {
            !l.terms
                .iter()
                .any(|(t, _)| t.name().is_some_and(|n| written.contains(&n)))
        };
        if self.kind(n) == Kind::Seq {
            let hole = Term::Val(Name(0));
            let ts: Vec<Lin> = (s.post.iter())
                .filter(|f| f.coef(hole) != 0)
                .filter_map(|f| {
                    f.map(|t| match t == hole {
                        true => Some(Lin::of(Term::Val(n))),
                        false => at(t),
                    })
                })
                .filter(free)
                .collect();
            if !ts.is_empty() {
                st.elems.entry(n).or_default().extend(ts);
            }
        }
        let facts: Vec<Lin> = s
            .post
            .iter()
            .filter_map(|f| f.map(at).filter(free))
            .collect();
        // A sentinel's facts wait in the result's slot of `conds` for a
        // comparison that shows it at least zero ([`Walk::sentinel`]).
        let when: Vec<Fact> = (s.when.iter())
            .filter_map(|f| f.map(at).filter(free))
            .map(Fact::Ge)
            .collect();
        if !when.is_empty() && self.kind(n).is_int() {
            st.conds.insert(n, (when, Vec::new()));
        }
        // A Bool result's facts hold where it is true.
        if self.body.names[n.index()].ty == Type::Bool {
            st.conds
                .insert(n, (facts.into_iter().map(Fact::Ge).collect(), Vec::new()));
            return;
        }
        // A result the summary pins to one value is defined as that value, so
        // a goal spends no premise on it.
        let r = Term::Val(n);
        let pinned = (facts.iter())
            .find(|f| f.terms == [(r, 1)] && f.scale(-1).is_some_and(|g| facts.contains(&g)));
        match pinned {
            Some(f) => {
                let c = Lin::k(-f.c);
                st.kill(n);
                st.define(r, &c);
            }
            None => facts.iter().for_each(|l| st.assume(l)),
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
        let Some((i, s)) = self.sums.call(rhs).and_then(|g| self.sums.get(g)) else {
            return;
        };
        let Rhs::Call { args, .. } = rhs else { return };
        if s.pre.is_empty() {
            return;
        }
        let at = |t: Term| {
            let k = t.name()?.index().checked_sub(1)?;
            let a = &args.get(k)?.0;
            match t {
                Term::Field(_, i) | Term::Col(_, i) => self.field_arg(t, i, a, s.types.get(k)?),
                _ => self.arg(t, a, *s.params.get(k)?),
            }
        };
        let holds = |g: &Lin| st.ge0(g).is_some_and(|cert| cert.verify(st, g));
        let seq = |j: usize| {
            j.checked_sub(1)
                .is_some_and(|k| s.params.get(k) == Some(&Kind::Seq))
        };
        // A template holds where the argument's own templates imply it.
        let met = |f: &Lin, j: Name| {
            let Some(a) = (args.get(j.index() - 1)).and_then(|(a, _)| root(a)) else {
                return false;
            };
            let a = self.arrays.get(&a).copied().unwrap_or(a);
            let hole = Term::Val(j);
            let want = f.map(|t| match t == hole {
                true => Some(Lin::of(Term::Val(a))),
                false => at(t),
            });
            want.is_some_and(|w| self.implied(st, a, &w))
        };
        let kept = (self.entered.get(&i).unwrap_or(&s.pre).iter())
            .filter(|f| match hole(f, seq) {
                Some(j) => met(f, j),
                None => f.map(at).is_some_and(|g| holds(&g)),
            })
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

    /// The field term `t`, field `i` of a record parameter of type `ty`, as the
    /// call's argument `a` gives it: a name of the same type.
    fn field_arg(&self, t: Term, i: u32, a: &Arg, ty: &Type) -> Option<Lin> {
        let (Arg::Val(Val::Name(m)) | Arg::Place(Place::Name(m))) = a else {
            return None;
        };
        let resolved = |x| vyrn_frontend::types::resolved(x, self.decls);
        if resolved(&self.body.names[m.index()].ty) != resolved(ty) {
            return None;
        }
        Some(Lin::of(match t {
            Term::Field(..) => Term::Field(*m, i),
            _ => Term::Col(*m, i),
        }))
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

    /// Keeps the summary candidates the state proves of the returned `value`;
    /// for a Bool, where it is true ([`Walk::truth`]).
    fn returned(&mut self, st: &State, value: Option<&Val>) {
        self.returned_when(st, value);
        if let Some((mut result, mut own)) = self.rows.take() {
            if !st.dead {
                let array = match value {
                    Some(Val::Name(m)) => self.array_of(&Place::Name(*m)),
                    _ => None,
                };
                for (k, p) in self.body.params.iter().enumerate() {
                    let held = |a: Name| {
                        (ghost_pair(k, *p).iter())
                            .filter_map(|t| renamed(t, *p, a))
                            .all(|t| st.elems.get(&a).is_some_and(|ts| ts.contains(&t)))
                    };
                    result[k] &= array.is_some_and(held);
                    own[k] &= held(*p);
                }
            }
            self.rows = Some((result, own));
        }
        let Some(cands) = self.post.take() else {
            return;
        };
        let truth = self.truth(st, value);
        let st = truth.as_ref().unwrap_or(st);
        // A `None` proves every payload fact, as a dead state does.
        let none = matches!(value, Some(Val::Name(m)) if !cands.is_empty() && self.only_none(*m));
        if st.dead || none {
            self.post = Some(cands);
            return;
        }
        let at = |t: Term| -> Option<Lin> {
            match (t.name()?.index().checked_sub(1), t, value?) {
                (None, Term::Val(_), Val::Name(m)) if self.payload(*m) => {
                    Some(Lin::of(Term::Val(*m)))
                }
                (None, Term::Field(_, i), Val::Name(m)) if self.payload_fields(*m).contains(&i) => {
                    Some(Lin::of(Term::Field(*m, i)))
                }
                (None, Term::Val(_), v) => self.lin(v),
                (None, Term::Len(_), Val::Name(m)) => Some(Lin::of(self.length(&Place::Name(*m))?)),
                (None, Term::Col(_, i), Val::Name(m)) if self.kind(*m) == Kind::Other => {
                    Some(Lin::of(Term::Col(*m, i)))
                }
                (Some(_), ..) => self.param_term(t),
                _ => None,
            }
        };
        let holds = |g: &Lin| st.ge0(g).is_some_and(|cert| cert.verify(st, g));
        // An array result's templates hold where its own imply them.
        let array = match value {
            Some(Val::Name(m)) if self.kind(*m) == Kind::Seq => self.array_of(&Place::Name(*m)),
            _ => None,
        };
        let hole = Term::Val(Name(0));
        let met = |c: &Lin| {
            let Some(a) = array else { return false };
            let want = c.map(|t| match t == hole {
                true => Some(Lin::of(Term::Val(a))),
                false => at(t),
            });
            want.is_some_and(|w| self.implied(st, a, &w))
        };
        let kept = (cands.into_iter())
            .filter(|c| match array.is_some() && c.coef(hole) != 0 {
                true => met(c),
                false => c.map(at).is_some_and(|g| holds(&g)),
            })
            .collect();
        self.post = Some(kept);
    }

    /// Keeps the `when` candidates the state proves where the returned
    /// integer `value` is at least zero: all of them where it cannot be.
    fn returned_when(&mut self, st: &State, value: Option<&Val>) {
        let Some(cands) = self.when.take() else {
            return;
        };
        let mut yes = st.clone();
        if let Some(v) = value.and_then(|v| self.lin(v)) {
            yes.add(&Fact::Ge(v));
        }
        let holds = |g: &Lin| yes.ge0(g).is_some_and(|cert| cert.verify(&yes, g));
        let kept = (cands.into_iter())
            .filter(|c| yes.dead || c.map(|t| self.param_term(t)).is_some_and(|g| holds(&g)))
            .collect();
        self.when = Some(kept);
    }

    /// A summary's term of parameter `k` (`Name(k + 1)`) as the body's own.
    fn param_term(&self, t: Term) -> Option<Lin> {
        let p = *self.body.params.get(t.name()?.index().checked_sub(1)?)?;
        Some(Lin::of(match t {
            Term::Val(_) => Term::Val(p),
            Term::Len(_) => Term::Len(p),
            Term::Field(_, i) => Term::Field(p, i),
            Term::Col(_, i) => Term::Col(p, i),
            Term::Inner(..) | Term::Ghost(..) => return None,
        }))
    }

    /// The state where the returned Bool `value` is true: none where it is
    /// `false`, `st` with the facts its truth gives for a name. `None` for
    /// any other value, whose facts are the state's.
    fn truth(&self, st: &State, value: Option<&Val>) -> Option<State> {
        match value? {
            Val::Lit(Lit::Bool(false)) => Some(State::dead()),
            Val::Name(m) if self.body.names[m.index()].ty == Type::Bool => {
                let mut yes = st.clone();
                (st.conds.get(m).into_iter().flat_map(|c| &c.0)).for_each(|f| yes.add(f));
                Some(yes)
            }
            _ => None,
        }
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
                if let Some(l) = self.lin(v).and_then(|l| Some(st.norm(&l)?.into_owned())) {
                    if copy || self.in_range(n, &l, st) {
                        st.define(Term::Val(n), &l);
                    }
                }
            }
            (Val::Name(m), Kind::Other) if self.payload(n) && self.payload(*m) => {
                st.define(Term::Val(n), &Lin::of(Term::Val(*m)));
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
        let p = st.prover();
        p.proves(&a) && p.proves(&b)
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
                    // `x & c` lies in `[0, c]` for a literal `c >= 0`, whatever `x` holds.
                    let v = Lin::of(Term::Val(n));
                    let mask = [&la, &lb].into_iter().find(|l| l.is_const() && l.c >= 0);
                    return match mask.filter(|_| *o == BinOp::BitAnd) {
                        Some(m) => {
                            Out::Facts([Some(v.clone()), m.sub(&v)].into_iter().flatten().collect())
                        }
                        None => Out::Nothing,
                    };
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
                let (t, f) = (self.sentinel(pre, a, b, t), self.sentinel(pre, a, b, f));
                let norm = |fs: Vec<Option<Fact>>| {
                    fs.into_iter()
                        .flatten()
                        .filter_map(|f| f.map(|l| Some(pre.norm(l)?.into_owned())))
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
            (Op::Conv(_), [a]) => {
                match self.lin(a).and_then(|l| Some(pre.norm(&l)?.into_owned())) {
                    Some(l) if self.in_range(n, &l, pre) => Out::Def(l),
                    _ => Out::Nothing,
                }
            }
            _ => Out::Nothing,
        }
    }

    /// The range of `n = a op b` where `b` is a literal that bounds it, a
    /// divisor of a non-negative `a % b` the state proves at least one, an
    /// operand of `&` the state proves non-negative, or the shift amount of a
    /// non-negative `a`.
    fn bound(&self, pre: &State, n: Name, o: BinOp, la: &Lin, lb: &Lin) -> Out {
        let p = pre.prover();
        let holds = |g: &Lin| p.ge0(g).is_some_and(|c| c.verify(pre, g));
        let least = |l: &Lin| p.least(l);
        let nonneg = holds(la);
        let v = Lin::of(Term::Val(n));
        // For `a` in `[0, h]`, `a / d` and `a >> k` with `k >= s` lie in
        // `[0, a]` and below `h / d` and `h >> s`. A `k` past the width traps.
        let quotient = |cut: &dyn Fn(i64) -> i64| {
            let h = la.scale(-1).and_then(|l| least(&l)).map(|m| cut(-m));
            [
                Some(v.clone()),
                la.sub(&v),
                h.and_then(|h| Lin::k(h).sub(&v)),
            ]
        };
        let facts = match (o, lb.is_const().then_some(lb.c)) {
            (BinOp::BitAnd, _) => match [lb, la].into_iter().find(|m| holds(m)) {
                Some(m) => [Some(v.clone()), m.sub(&v), None],
                None => return Out::Nothing,
            },
            (BinOp::Rem, _) if nonneg && lb.plus(-1).is_some_and(|d| holds(&d)) => {
                [Some(v.clone()), lb.plus(-1).and_then(|d| d.sub(&v)), None]
            }
            (BinOp::Div, Some(k)) if k >= 1 && nonneg => quotient(&|h| h / k),
            (BinOp::Shr, _) if nonneg => match least(lb) {
                Some(s) if s >= 0 => quotient(&|h| h >> s.min(63)),
                _ => return Out::Nothing,
            },
            _ => return Out::Nothing,
        };
        Out::Facts(facts.into_iter().flatten().collect())
    }

    /// `a && b` or `a || b`: the side that gives both operands' facts.
    /// `side`, the facts one side of a comparison of `a` and `b` gives, with
    /// the facts a sentinel's call left for `a` or `b` ([`Summary::when`])
    /// where `side` alone shows it at least zero: `c == '('`, or `c < 0`
    /// false.
    fn sentinel(
        &self,
        pre: &State,
        a: &Val,
        b: &Val,
        side: Vec<Option<Fact>>,
    ) -> Vec<Option<Fact>> {
        let mut out = side;
        for v in [a, b] {
            let Val::Name(m) = v else { continue };
            let Some((when, _)) = pre.conds.get(m).filter(|_| self.kind(*m).is_int()) else {
                continue;
            };
            let mut probe = State::default();
            out.iter().flatten().for_each(|f| probe.add(f));
            let at_least_zero = Lin::of(Term::Val(*m));
            if probe
                .ge0(&at_least_zero)
                .is_some_and(|c| c.verify(&probe, &at_least_zero))
            {
                out.extend(when.iter().cloned().map(Some));
            }
        }
        out
    }

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

    /// The length term of the receiver of a builtin call in `rhs` whose row
    /// states its length effect, `None` for a String literal, and the bounds
    /// of its new length as sums over the old.
    fn resized(&self, rhs: &Rhs) -> Option<(Option<Term>, Lin, Lin)> {
        let Rhs::Call {
            callee,
            args,
            kind: Callee::Builtin | Callee::Reserved,
            ..
        } = rhs
        else {
            return None;
        };
        let term = |i: usize| match args.get(i)? {
            (Arg::Val(Val::Name(n)), _) => self.length(&Place::Name(*n)),
            (Arg::Place(p), _) => self.length(p),
            _ => None,
        };
        let len = |i: usize| match args.get(i)? {
            (Arg::Val(Val::Lit(Lit::Str(x))), _) => Some(Lin::k(x.len() as i64)),
            _ => term(i).map(Lin::of),
        };
        let old = len(0)?;
        let (lo, hi) = match length_rule(callee, args) {
            Length::Unknown => return None,
            Length::Keeps => (old.clone(), old),
            Length::GrowsByOne => (old.plus(1)?, old.plus(1)?),
            Length::ShrinksByOneIfNotEmpty => (old.plus(-1)?, old),
            Length::SetToZero => (Lin::k(0), Lin::k(0)),
            Length::GrowsByLenOf(i) => {
                let sum = old.add(&len(i)?)?;
                (sum.clone(), sum)
            }
            Length::SetToLenOf(i) => {
                let l = len(i)?;
                (l.clone(), l)
            }
        };
        Some((term(0), lo, hi))
    }

    /// Applies what a call in `rhs` does to the names it may write. A
    /// `modify` receiver moves its length as its row states; any other
    /// `modify` or `consume` argument rooted at a name that is not a scalar
    /// forgets it.
    fn effects(&mut self, st: &mut State, rhs: &Rhs) {
        let Rhs::Call { args, kind, .. } = rhs else {
            return;
        };
        // Only a builtin that writes no argument writes no element: any
        // other call may write one through an argument or module state.
        let pure = matches!(kind, Callee::Builtin | Callee::Reserved)
            && args.iter().all(|(_, c)| *c == Capability::Read);
        if !pure {
            st.forget_inner();
        }
        for (a, cap) in args {
            if let (Arg::Place(p), Capability::Modify) = (a, cap) {
                self.open(st, p);
            }
        }
        // A `modify` builtin that may change an element keeps no template of it.
        if let (Callee::Builtin | Callee::Reserved, Rhs::Call { callee, .. }) = (kind, rhs) {
            let keeps = prelude::builtin(callee).is_some_and(|r| {
                matches!(
                    r.elements,
                    Elements::KeepsEachPosition | Elements::KeepsRange
                )
            });
            if !keeps {
                for (a, _) in args.iter().filter(|(_, c)| *c == Capability::Modify) {
                    if let Some(n) = root(a) {
                        st.elems.remove(&n);
                    }
                }
            }
        }
        let moved = self.resized(rhs).filter(|_| !lands_on_result(args));
        for (i, (a, cap)) in args.iter().enumerate() {
            let Some(n) = root(a) else { continue };
            match (cap, &moved) {
                (Capability::Read, _) => {}
                // A scalar argument is a copy.
                (Capability::Consume, _) if matches!(self.kind(n), Kind::Int(..)) => {}
                (Capability::Modify, Some((Some(t), lo, hi))) if i == 0 => {
                    let old = Lin::of(*t);
                    match (lo.sub(&old), hi.sub(&old)) {
                        (Some(a), Some(b)) if a.is_const() && b.is_const() => {
                            // `ShrinksByOneIfNotEmpty` shrinks a length of at
                            // least one by exactly one.
                            let full = old.plus(-1).is_some_and(|g| {
                                (st.prover().ge0_deep(&g)).is_some_and(|c| c.verify(st, &g))
                            });
                            match (a.c, b.c) {
                                (-1, 0) if full => st.shift(*t, -1, Some(-1)),
                                (a, b) => st.shift(*t, a, Some(b)),
                            }
                        }
                        _ => self.resize(st, n),
                    }
                }
                (Capability::Modify, _)
                    if matches!(a, Arg::Val(_) | Arg::Place(Place::Name(_))) =>
                {
                    match self.sums.frame(rhs, i).filter(|f| !f.moves.is_empty()) {
                        Some(f) => self.frame(st, n, f),
                        None => self.resize(st, n),
                    }
                }
                _ => self.resize(st, n),
            }
        }
        for (k, (a, cap)) in args.iter().enumerate() {
            match (a, cap) {
                (Arg::Place(Place::Field(r, f)), Capability::Modify | Capability::Consume) => {
                    if let Place::Name(r) = &**r {
                        self.wrote_field(st, *r, f);
                    }
                }
                // A declared function proves the facts of its `modify`
                // parameter's type at every exit. The argument's type may
                // have more facts; then the walk tracks its fields.
                (Arg::Val(Val::Name(r)) | Arg::Place(Place::Name(r)), Capability::Modify) => {
                    let theirs = match kind {
                        Callee::Fn(g) => self.sums.facts.param(*g, k),
                        _ => None,
                    };
                    let kept = |f: &RecordFact| theirs.is_some_and(|t| t.contains(f));
                    match self.facts_of(*r).iter().all(|(_, f)| kept(f)) {
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
            let Some(k0) = entry.norm(&Lin::of(Term::Val(k))).map(Cow::into_owned) else {
                continue;
            };
            let Some(end) = s.checked_mul(n).and_then(|d| k0.plus(d)) else {
                continue;
            };
            if k == u
                || s == 0
                || writes_of(body, k) != 1
                || !matches!(self.kind(k), Kind::Int(..))
                || k0
                    .terms
                    .iter()
                    .any(|(t, _)| t.name().is_some_and(|n| written.contains(&n)))
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
        if !self.borrows.is_empty() {
            for (s, _) in rows(body) {
                self.stale(s).iter().for_each(|b| _ = head.fresh.remove(b));
            }
        }
        for l in self.halving(entry, body, &written) {
            head.assume(&l);
        }
        // Candidates: what entry knows about the written names, two fields
        // of a record it grows that entry defines equally long, the record
        // facts of a written record whose fields a row set, and the counter
        // shapes an index needs.
        let mut cands: BTreeSet<Lin> = BTreeSet::new();
        for f in &entry.facts {
            if f.terms
                .iter()
                .any(|(t, _)| t.name().is_some_and(|n| written.contains(&n)))
            {
                cands.insert(f.clone());
            }
        }
        for (t, v) in &entry.defs {
            let mentions = t.name().is_some_and(|n| written.contains(&n))
                || v.terms
                    .iter()
                    .any(|(x, _)| x.name().is_some_and(|n| written.contains(&n)));
            if mentions {
                cands.extend(Lin::of(*t).sub(v));
                cands.extend(v.sub(&Lin::of(*t)));
            }
        }
        cands.extend(equal_fields(entry, &written));
        for n in written.iter().filter(|n| self.dirty.contains(n)) {
            cands.extend(self.facts_of(*n).into_iter().map(|(l, _)| l));
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
            // A counter stored as a remainder by a literal `c` stays below
            // `c` after a turn, and at most `c` where it entered at most `c`:
            // an `Int64` hash or random state then multiplies exactly.
            let rem = |s: &St| match s {
                St::Store {
                    place: Place::Name(x),
                    value: Val::Name(t),
                    ..
                } if x == m => match let_of(body, *t) {
                    Some(Rhs::Prim(Op::Bin(BinOp::Rem), vs, _)) => match vs.as_slice() {
                        [_, Val::Lit(Lit::Int(c))] if *c > 0 => Some(*c),
                        _ => None,
                    },
                    _ => None,
                },
                _ => None,
            };
            for c in rows(body).filter_map(|(s, _)| rem(s)) {
                cands.extend(Lin::k(c).sub(&v));
                cands.extend(Lin::k(c - 1).sub(&v));
            }
            // An index `m - c` by a literal `c` needs `m - c >= 0`, so a
            // counter that steps down to `c` keeps it.
            let below = |i: &Name| match let_of(body, *i) {
                Some(Rhs::Prim(Op::Bin(BinOp::Sub), vs, _)) => match vs.as_slice() {
                    [Val::Name(x), Val::Lit(Lit::Int(c))] if x == m && *c > 0 => Some(*c),
                    _ => None,
                },
                _ => (i == m).then_some(0),
            };
            for (b, c) in indexed.iter().filter_map(|(b, i)| Some((b, below(i)?))) {
                let Some(len) = self.head_length(body, b) else {
                    continue;
                };
                let Some(at) = v.plus(-c) else { continue };
                if c == 0 {
                    cands.extend(len.sub(&at));
                } else {
                    cands.insert(at.clone());
                }
                cands.extend(len.sub(&at).and_then(|l| l.plus(-1)));
            }
        }
        cands.extend(self.reaches(body, &stored, &indexed));
        cands.extend(self.in_step(body, &stored));
        let copies = self.copies(body, &stored);
        for (k, m) in &copies {
            let d = Lin::of(Term::Val(*k)).sub(&Lin::of(Term::Val(*m)));
            cands.extend(d.as_ref().and_then(|d| d.plus(-1)));
            cands.extend(d);
        }
        if !cands.is_empty() {
            let at_entry = entry.prover();
            cands.retain(|c| at_entry.proves(c));
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
        // Each element template at entry is a candidate: an element store
        // writes no name, so the head would hold the template unchecked. A
        // turn keeps one where every end still holds it.
        head.elems.clear();
        let mut temps: Vec<(Name, Lin)> = (entry.elems.iter())
            .flat_map(|(a, ts)| ts.iter().map(move |t| (*a, t.clone())))
            .collect();
        let held = |h: &mut State, temps: &[(Name, Lin)]| {
            for (a, t) in temps {
                h.elems.entry(*a).or_default().insert(t.clone());
            }
        };
        let record = std::mem::replace(&mut self.record, false);
        loop {
            let mut h = head.clone();
            cands.iter().for_each(|c| h.assume(c));
            held(&mut h, &temps);
            let end = self.block(h, body);
            let l = self.loops.last_mut().expect("pushed above");
            l.breaks.clear();
            let conts = std::mem::take(&mut l.conts);
            let ends: Vec<State> = conts.into_iter().chain([end]).collect();
            if cands.is_empty() && temps.is_empty() {
                break;
            }
            let before = (cands.len(), temps.len());
            let provers: Vec<_> = ends.iter().map(|s| s.prover()).collect();
            cands.retain(|c| provers.iter().all(|p| p.proves(c)));
            let live = ends.iter().filter(|e| !e.dead);
            let kept = |(a, t): &(Name, Lin)| {
                live.clone()
                    .all(|e| e.elems.get(a).is_some_and(|x| x.contains(t)))
            };
            temps.retain(kept);
            if (cands.len(), temps.len()) == before {
                break;
            }
        }
        self.record = record;
        cands.iter().for_each(|c| head.assume(c));
        held(&mut head, &temps);
        self.block(head, body);
        let mut l = self.loops.pop().expect("pushed above");
        if let Some(outer) = self.loops.last_mut() {
            outer.seen.resized.extend(l.seen.resized);
        }
        let mut exit = State::join(&mut l.breaks);
        // The counter of a loop bound by a literal is at least the bound at
        // the exit. A length the counter bounds then has the literal as its
        // own bound, which a goal would otherwise need a third premise for.
        // A counter that steps up to a length equals it at the exit, where
        // the head kept it at most the length ([`Walk::in_step`]), so a
        // length the counter bounds from either side has the bound's.
        if let Some((_, k, b)) = bound_test(body).filter(|(_, k, b)| {
            (b.is_const() || measured(body, b).is_some()) && self.slot[k.index()].relevant
        }) {
            let k = Term::Val(k);
            let len = (measured(body, &b))
                .and_then(|p| self.head_length(body, p))
                .and_then(|l| Some(exit.norm(&l)?.into_owned()));
            let (low, high) = match len {
                None => {
                    let low = (exit.facts.iter()).find(|f| f.terms == [(k, 1)]).cloned();
                    (low, None)
                }
                Some(l) => {
                    let held = |f: Option<Lin>| f.filter(|f| exit.facts.contains(f));
                    let low = held(Lin::of(k).sub(&l));
                    (low, held(l.sub(&Lin::of(k))))
                }
            };
            let length = |f: &Lin| (f.terms.iter()).any(|(t, _)| matches!(t, Term::Len(_)));
            // A bound the state already holds as one fact adds nothing.
            let held = |f: &Lin| (exit.facts.iter()).any(|g| g.terms == f.terms && g.c <= f.c);
            let bounded: Vec<Lin> = (exit.facts.iter())
                .filter(|f| f.terms.len() == 2 && length(f))
                .filter_map(|f| match f.coef(k) {
                    c if c < 0 => f.add(&low.as_ref()?.scale(-c)?),
                    c if c > 0 => f.add(&high.as_ref()?.scale(c)?),
                    _ => None,
                })
                .filter(|f| !f.is_const() && !held(f))
                .collect();
            bounded.iter().for_each(|f| exit.assume(f));
        }
        // A copy below the counter lies below the counter's own bound:
        // `i - at - 1 >= 0` and `n - i >= 0` give `n - at - 1 >= 0`.
        for (k, m) in copies {
            let (k, m) = (Term::Val(k), Term::Val(m));
            let below = |f: &&Lin| f.terms.len() == 2 && f.coef(k) == 1 && f.coef(m) == -1;
            let lows: Vec<&Lin> = exit.facts.iter().filter(below).collect();
            let length = |f: &&Lin| (f.terms.iter()).any(|(t, _)| matches!(t, Term::Len(_)));
            let tops = (exit.facts.iter()).filter(|f| f.terms.len() == 2 && f.coef(k) == -1);
            let bounded: Vec<Lin> = (tops.filter(length))
                .flat_map(|t| lows.iter().filter_map(|l| t.add(l)))
                .collect();
            bounded.iter().for_each(|f| exit.assume(f));
        }
        exit
    }

    /// `a - b` and its negation, where `a` and `b` are the lengths of two
    /// arrays or Strings the loop `body` stores to on the same paths, names
    /// or fields of a record name, both of which a check can depend on: a
    /// loop that grows them in step keeps them as equal as it entered. The
    /// counter of a loop with a bound ([`bound_test`]) takes part as its
    /// value; in a walk that proves a result, only for a literal, parameter
    /// or length bound: any bound there cost 1.1% more `elide: summaries`
    /// allocations on the site export and proved no row. [`Walk::settle`]
    /// keeps a candidate only where the entry and every turn prove it.
    fn in_step(&self, body: &[St], stored: &BTreeSet<Name>) -> Vec<Lin> {
        let relevant = |n: &Name| self.slot[n.index()].relevant;
        let param =
            |b: &Lin| matches!(b.terms[..], [(Term::Val(p), 1)] if self.body.params.contains(&p));
        let test = bound_test(body).filter(|(_, k, _)| relevant(k));
        let counter = (test.as_ref())
            .filter(|(.., b)| {
                self.post.is_none() || b.is_const() || param(b) || measured(body, b).is_some()
            })
            .map(|(_, k, _)| *k);
        let term = |(n, f): &(Name, Option<&str>)| match (f, Some(*n) == counter) {
            (None, true) => Some(Term::Val(*n)),
            (None, false) => (relevant(n) && self.kind(*n) == Kind::Seq).then_some(Term::Len(*n)),
            (Some(f), _) => relevant(n).then(|| self.col_term(*n, f)).flatten(),
        };
        let named = stored.iter().filter_map(|n| term(&(*n, None))).count();
        let field = || {
            rows(body).any(|(s, _)| match s {
                St::Store {
                    place: Place::Field(r, f),
                    ..
                } => matches!(**r, Place::Name(r) if term(&(r, Some(f))).is_some()),
                _ => false,
            })
        };
        if named == 0 || named == 1 && !field() {
            return Vec::new();
        }
        let mut paths = BTreeMap::new();
        store_paths(body, &mut Vec::new(), &mut paths);
        let seqs: Vec<(Term, &Vec<Vec<u32>>)> = paths
            .iter()
            .filter_map(|(n, p)| Some((term(n)?, p)))
            .collect();
        let mut out = Vec::new();
        // A counter bound by a length it steps up to stays at most that
        // length: `while i < xs.length { ys.push(..) ; i = i + 1 }` leaves
        // `ys` as long as `xs`.
        if let (Some(k), Some((.., b))) = (counter, &test) {
            let len = measured(body, b).and_then(|p| self.head_length(body, p));
            out.extend(len.and_then(|l| l.sub(&Lin::of(Term::Val(k)))));
        }
        for (i, (a, pa)) in seqs.iter().enumerate() {
            for (b, pb) in &seqs[i + 1..] {
                if pa == pb {
                    let d = Lin::of(*a).sub(&Lin::of(*b));
                    out.extend(d.as_ref().and_then(|d| d.scale(-1)));
                    out.extend(d);
                }
            }
        }
        out
    }

    /// Each `(k, m)` where `body` stores `m = k`, `k` a counter the loop
    /// stores to: `at = i` keeps `at < i` once `i` steps past, a candidate
    /// at the head and a bound at the exit.
    fn copies(&self, body: &[St], stored: &BTreeSet<Name>) -> Vec<(Name, Name)> {
        let int = |n: &Name| self.slot[n.index()].relevant && self.kind(*n).is_int();
        let mut out = Vec::new();
        for (s, _) in rows(body) {
            if let St::Store {
                place: Place::Name(m),
                value: Val::Name(k),
                ..
            } = s
            {
                if m != k && stored.contains(k) && int(m) && int(k) {
                    out.push((*k, *m));
                }
            }
        }
        out
    }

    /// `len(b) - m - c`, for each index of `b` in `body` that a `let`
    /// defines as `m + x` ([`reach`]), `m` a counter the loop stores to: the
    /// counter shape of a row offset `m` and a column `x` below `c`.
    fn reaches(&self, body: &[St], stored: &BTreeSet<Name>, indexed: &[(Place, Name)]) -> Vec<Lin> {
        let def = |n: Name| let_of(body, n);
        let mut out = Vec::new();
        for (b, i) in indexed {
            let Some((m, c)) = def(*i).and_then(|d| reach(self.body, self.decls, d, &def)) else {
                continue;
            };
            if stored.contains(&m) && matches!(self.kind(m), Kind::Int(..)) {
                let len = self.head_length(body, b);
                out.extend(len.and_then(|l| l.sub(&Lin::of(Term::Val(m)))?.plus(-c)));
            }
        }
        out
    }

    /// Moves the slots of `n`, a whole `modify` argument, as the callee's
    /// frame states ([`frames`]): a kept slot keeps its facts, one that grows
    /// keeps those that bound it from below, and any other forgets them. A
    /// field term of a class of equal lengths moves as its fields' join.
    fn frame(&mut self, st: &mut State, n: Name, frame: &Frame) {
        let moves = frame.moves.as_slice();
        let ty = &self.body.names[n.index()].ty;
        let fields = vyrn_frontend::types::record_fields(ty, self.decls);
        let class = |i: u32| -> Moves {
            let Some(fs) = fields.as_deref() else {
                return Moves::Any;
            };
            if self.rule(n).is_none() || self.open.contains(&n) {
                return moves.get(i as usize).copied().unwrap_or(Moves::Any);
            }
            (fs.iter().zip(moves))
                .filter(|(f, _)| self.col(n, &f.name).is_some_and(|(_, l)| l == i))
                .map(|(_, m)| *m)
                .max()
                .unwrap_or(Moves::Any)
        };
        let mut moved = false;
        for t in st.terms_of(n) {
            let m = match t {
                Term::Len(_) if fields.is_none() => moves.first().copied().unwrap_or(Moves::Any),
                Term::Field(_, i) => moves.get(i as usize).copied().unwrap_or(Moves::Any),
                Term::Col(_, i) => class(i),
                // An array's value is the hole of its element templates.
                Term::Val(_) if fields.is_none() && frame.own => Moves::Keeps,
                Term::Val(_) | Term::Len(_) | Term::Inner(..) | Term::Ghost(..) => Moves::Any,
            };
            match m {
                Moves::Keeps => {}
                Moves::Grows => st.shift(t, 0, None),
                Moves::Any => st.forget(t),
            }
            moved |= m != Moves::Keeps;
        }
        if moved {
            st.fresh.remove(&n);
            if let Some(l) = self.loops.last_mut() {
                l.seen.resized.insert(n);
            }
        }
    }

    /// The length of `b`, an index base in the loop `body`, as a fact at the
    /// loop's head can name it: a borrow the body binds (`let @borrow = read
    /// xs`) stands for its source.
    fn head_length(&self, body: &[St], b: &Place) -> Option<Lin> {
        let src = match b {
            Place::Name(x) => match let_of(body, *x) {
                Some(Rhs::Read(p @ Place::Name(_))) => p,
                _ => b,
            },
            _ => b,
        };
        self.length(src).map(Lin::of)
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
            (None, Guard::Range(s, a, b)) => {
                if val_name(s).is_some_and(|s| self.length(&Place::Name(s)).is_none()) {
                    return Why::Place(val_name(s));
                }
                [val_name(a), val_name(b)]
            }
            (None, Guard::Index(p, i) | Guard::Span(p, i, _)) => {
                if self.length(p).is_none() {
                    return Why::Place(place_root(p));
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
                .filter_map(|(t, _)| t.name())
                .chain(operands.into_iter().flatten())
        };
        let goal = || failing.map(|g| self.goal(st, g, None).0);
        let origin = names().fold(Origin::Local, |o, n| o.join(self.slot[n.index()].origin));
        match origin {
            Origin::Input(n) => return Why::Input(n),
            Origin::Call(n, f) => return Why::Callee(n, f, goal()),
            Origin::Place(n) => return Why::Place(Some(n)),
            _ => {}
        }
        let resized = |n: Name| self.loops.iter().any(|l| l.seen.resized.contains(&n));
        let grown = |(t, _): &(Term, i64)| {
            t.name()
                .filter(|n| !matches!(t, Term::Val(_)) && resized(*n))
        };
        if let Some(n) = terms().find_map(grown) {
            return Why::Resized(n);
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
                return Why::Counter(n, goal());
            }
        }
        match (origin, failing) {
            (Origin::Param(n), Some(g)) => {
                let (says, clause) = self.goal(st, g, Some(n));
                Why::Caller(n, says, clause)
            }
            (_, Some(g)) => Why::Unproved(self.goal(st, g, None).0),
            (_, None) if matches!(c.guard, Guard::Rule(_)) => Why::Rule,
            (_, None) => Why::Nonlinear,
        }
    }

    /// The goal `g >= 0`, over the names the state leaves free, as the source
    /// would test it: `` `xs` non-empty ``, `` `i < xs.length` ``. A goal over a
    /// temporary reads `the value of line N` and is no code. With `param`, also
    /// the goal as that parameter's `where` clause, where [`Walk::states`]
    /// allows one.
    fn goal(&self, st: &State, g: &Lin, param: Option<Name>) -> (Arc<str>, Option<Arc<str>>) {
        let l = &st.norm(g).unwrap_or(Cow::Borrowed(g));
        let temp = (l.terms.iter()).any(|(t, _)| {
            t.name()
                .is_some_and(|n| self.body.names[n.index()].source.starts_with('@'))
        });
        let says = match (l.terms.as_slice(), l.c, temp) {
            ([(Term::Len(n), 1)], -1, true) => format!("{} non-empty", self.body.spoken(*n)),
            ([(Term::Len(n), 1)], -1, false) => format!("`{}` non-empty", self.body.spoken(*n)),
            (_, _, true) => self.lin_text(l, None),
            (_, _, false) => format!("`{}`", self.lin_text(l, None)),
        };
        let clause = param.filter(|p| self.states(l, *p));
        (
            says.into(),
            clause.map(|p| self.lin_text(l, Some(p)).into()),
        )
    }

    /// Whether a `where` clause on the parameter `p` states `l >= 0`. The
    /// clause grammar (`docs/language.md`) wants a `read` parameter, sides of
    /// one atom or a literal, and atoms over `p` and the `read` parameters
    /// before it.
    fn states(&self, l: &Lin, p: Name) -> bool {
        let at = |n: Name| self.body.params.iter().position(|q| *q == n);
        let read = |n: Name| {
            matches!(
                &self.body.names[n.index()].borrow_kind,
                Some(BorrowKind::Param { cap: "read", .. })
            )
        };
        let Some(own) = at(p).filter(|_| read(p)) else {
            return false;
        };
        let (pos, neg) = (l.terms.iter()).partition::<Vec<&(Term, i64)>, _>(|(_, k)| *k > 0);
        let atoms = pos.len() <= 1
            && neg.len() <= 1
            && (pos.is_empty() || neg.is_empty() || l.c == 0 || l.c == -1);
        atoms
            && l.terms.iter().any(|(t, _)| t.name() == Some(p))
            && (l.terms.iter()).all(|(t, k)| {
                k.unsigned_abs() == 1
                    && !matches!(t, Term::Col(..) | Term::Inner(..))
                    && t.name()
                        .is_some_and(|n| n == p || (read(n) && at(n).is_some_and(|i| i < own)))
            })
    }

    /// `l >= 0` as a comparison with the smaller side first: `i < xs.length`,
    /// `n >= 1`. `value` stands for `param`.
    fn lin_text(&self, l: &Lin, param: Option<Name>) -> String {
        let atom = |t: Term| -> String {
            // A ghost is never a check's goal ([`element_rows`]).
            let Some(n) = t.name() else {
                return "a bound".into();
            };
            let name = match Some(n) == param {
                true => "value".into(),
                false => self.body.spoken(n),
            };
            let ty = &self.body.names[n.index()].ty;
            let len = |ty: &Type| {
                if *ty == Type::Str {
                    "byteLength"
                } else {
                    "length"
                }
            };
            let field = |i: u32| {
                vyrn_frontend::types::record_fields(ty, self.decls)
                    .and_then(|f| f.get(i as usize).map(|f| (f.name.clone(), f.ty.clone())))
            };
            match t {
                Term::Val(_) => name.into_owned(),
                Term::Len(_) => format!("{name}.{}", len(ty)),
                Term::Field(_, i) => match field(i) {
                    Some((f, _)) => format!("{name}.{f}"),
                    None => format!("a field of {name}"),
                },
                Term::Col(_, i) => match field(i) {
                    Some((f, fty)) => format!("{name}.{f}.{}", len(&fty)),
                    None => format!("the length of a field of {name}"),
                },
                Term::Inner(_, f, i) => {
                    let at = self.body.spoken(i);
                    match f.and_then(field) {
                        Some((f, _)) => format!("{name}.{f}[{at}].length"),
                        None => format!("{name}[{at}].length"),
                    }
                }
                Term::Ghost(..) => "a bound".into(),
            }
        };
        // The constant in `i128`, so negating `i64::MIN` cannot overflow.
        let sum = |ts: &[&(Term, i64)], c: i128| -> String {
            let mut out: Vec<String> = (ts.iter())
                .map(|(t, k)| match k.unsigned_abs() {
                    1 => atom(*t),
                    k => format!("{k} * {}", atom(*t)),
                })
                .collect();
            if c != 0 || out.is_empty() {
                out.push(c.to_string());
            }
            out.join(" + ")
        };
        let (pos, neg) = (l.terms.iter()).partition::<Vec<&(Term, i64)>, _>(|(_, k)| *k > 0);
        match (pos.is_empty(), neg.is_empty(), i128::from(l.c)) {
            (_, true, c) => format!("{} >= {}", sum(&pos, 0), -c),
            (true, false, c) => format!("{} <= {c}", sum(&neg, 0)),
            (false, false, c) if c < 0 => format!("{} < {}", sum(&neg, -c - 1), sum(&pos, 0)),
            (false, false, c) => format!("{} <= {}", sum(&neg, 0), sum(&pos, c)),
        }
    }
}

/// The array or String field `f` of the record type `ty`, by its own index and
/// by the least index among the fields the type's `where` rule states of
/// equal length. The rule holds wherever the record is checked, so one
/// term is the length of every field of a class ([`Term::Col`]).
fn col(ty: &Type, f: &str, decls: &HashMap<String, TypeDecl>) -> Option<(u32, u32)> {
    let seq = |t: &Type| {
        let t = vyrn_frontend::types::resolve(t, decls);
        t.is_seq() || t == Type::Str
    };
    let fields = vyrn_frontend::types::record_fields(ty, decls)?;
    let at = |g: &str| fields.iter().position(|x| x.name == g && seq(&x.ty));
    let own = at(f)?;
    let mut class = BTreeSet::from([own]);
    let rule = match ty {
        Type::Named(n) => decls.get(n).and_then(|d| d.predicate.as_ref()),
        _ => None,
    };
    let pairs = rule
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

/// The test name, the counter `k` and its bound `c` of a loop `body` that
/// opens with `if k < c else break`, or `if k <= c - 1 else break`, where
/// `c` is an integer literal or a name's value: `k` is below `c` on every
/// turn. A `let` that reads the bound as a length may come first
/// ([`measured`]).
fn bound_test(body: &[St]) -> Option<(Name, Name, Lin)> {
    let (body, measure) = match body {
        [St::Let(l, Rhs::Read(Place::Field(_, f))), rest @ ..]
            if f == "length" || f == "byteLength" =>
        {
            (rest, Some(*l))
        }
        _ => (body, None),
    };
    let [St::Let(t, Rhs::Prim(Op::Bin(op @ (BinOp::Lt | BinOp::LtEq)), vs, _)), St::If {
        cond: Val::Name(c),
        then,
        els,
        ..
    }, ..] = body
    else {
        return None;
    };
    let (k, bound) = match (vs.as_slice(), &then[..], &els[..]) {
        ([Val::Name(k), Val::Lit(Lit::Int(n))], [], [St::Break { .. }]) => (k, Lin::k(*n)),
        ([Val::Name(k), Val::Name(n)], [], [St::Break { .. }]) => (k, Lin::of(Term::Val(*n))),
        _ => return None,
    };
    if measure.is_some_and(|l| bound != Lin::of(Term::Val(l))) {
        return None;
    }
    let bound = bound.plus(i64::from(*op == BinOp::LtEq))?;
    (c == t).then_some((*t, *k, bound))
}

/// The place whose length the loop `body` reads as its bound `b` in its
/// first row ([`bound_test`]).
fn measured<'b>(body: &'b [St], b: &Lin) -> Option<&'b Place> {
    let [(Term::Val(n), 1)] = b.terms[..] else {
        return None;
    };
    match body.first() {
        Some(St::Let(m, Rhs::Read(Place::Field(base, f))))
            if *m == n && b.c == 0 && (f == "length" || f == "byteLength") =>
        {
            Some(base)
        }
        _ => None,
    }
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
/// The length a walk reads for a call to the builtin `callee` with `args`:
/// the row's ([`prelude::Builtin::length_at`]), except that a row that
/// builds its result from a name (`bytes(s)`) states nothing. Read from a
/// name, `bytes` proved 9 more static rows, run 342 times in all, for 1.6%
/// more `elide: summaries` allocations on the site export; from a String
/// literal (`bytes("..")`) it costs none.
fn length_rule(callee: &str, args: &[(Arg, Capability)]) -> Length {
    let Some(row) = prelude::builtin(callee) else {
        return Length::Unknown;
    };
    match (&row.spec, args.first()) {
        (Some(prelude::Spec::Builds(_)), Some((Arg::Val(Val::Name(_)) | Arg::Place(_), _))) => {
            Length::Unknown
        }
        _ => row.length_at(args.len()),
    }
}

fn lands_on_result(args: &[(Arg, Capability)]) -> bool {
    args.first().is_some_and(|(_, c)| *c != Capability::Modify)
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

/// The global `p` is a part of.
fn global_root(p: &Place) -> Option<&str> {
    match p {
        Place::Global(g) => Some(g),
        Place::Field(b, _) | Place::Elem(b, _) | Place::Key(b, _) => global_root(b),
        Place::Name(_) => None,
    }
}

/// Per global, the names a `let` of `ss` binds to the global's whole value
/// and no other row writes: each a borrow, equal to the global where it is
/// bound.
fn global_borrows(ss: &[St]) -> BTreeMap<String, Vec<Name>> {
    let mut out: BTreeMap<String, Vec<Name>> = BTreeMap::new();
    for (s, _) in rows(ss) {
        if let St::Let(n, Rhs::Read(Place::Global(g))) = s {
            if writes_of(ss, *n) == 1 {
                out.entry(g.clone()).or_default().push(*n);
            }
        }
    }
    out
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
/// comparison, a check or a `linked` call row links to a relevant one,
/// either way.
fn relevant(
    body: &Body,
    decls: &HashMap<String, TypeDecl>,
    ss: &[St],
    returns: bool,
    checks: bool,
    linked: &dyn Fn(&Rhs) -> bool,
    entering: &dyn Fn(FnId, usize) -> bool,
    arrays: &BTreeSet<Name>,
) -> Vec<bool> {
    let mut rel = vec![false; body.names.len()];
    arrays.iter().for_each(|a| rel[a.index()] = true);
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
    let goals = match checks {
        true => Goals::Checks,
        false => Goals::Results,
    };
    loop {
        let before = rel.iter().filter(|r| **r).count();
        mark(body, decls, ss, &mut rel, goals, linked, arrays);
        if rel.iter().filter(|r| **r).count() == before {
            return rel;
        }
    }
}

/// What a walk proves, for [`mark`]: its kept checks, the record facts its
/// rows oblige ([`obliged`]), or only the facts of its results (a solve).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Goals {
    Checks,
    Obligations,
    Results,
}

fn mark(
    body: &Body,
    decls: &HashMap<String, TypeDecl>,
    ss: &[St],
    rel: &mut [bool],
    goals: Goals,
    linked: &dyn Fn(&Rhs) -> bool,
    arrays: &BTreeSet<Name>,
) {
    let val = |v: &Val| match v {
        Val::Name(n) => Some(*n),
        Val::Lit(_) => None,
    };
    for s in ss {
        match s {
            St::Check(c) => {
                // A kept check's names are relevant; any other check links them.
                let mut any = goals == Goals::Checks && c.verdict == Verdict::Kept;
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
                    // An array whose element templates the walk keeps holds
                    // its literal's parts to them.
                    Rhs::Make(Ctor::Array, vs) if n.is_some_and(|n| arrays.contains(&n)) => {
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
                            && length_rule(callee, args) != Length::Unknown;
                        if lengths || linked(rhs) {
                            let from = args.iter().filter_map(|(a, _)| root(a));
                            link(rel, n.into_iter().chain(from));
                        }
                        link(rel, n.into_iter().chain(some_part(rhs)));
                        // A solve states the integer fields of a payload a
                        // literal builds ([`templates`]).
                        let made = (some_part(rhs).filter(|_| goals == Goals::Results))
                            .and_then(|v| let_of(&body.stmts, v));
                        if let Some(Rhs::Make(Ctor::Record(..), vs)) = made {
                            let int = |m: &Name| kind_of(&body.names[m.index()].ty, decls).is_int();
                            link(
                                rel,
                                n.into_iter().chain(vs.iter().filter_map(val).filter(int)),
                            );
                        }
                    }
                    _ => {}
                }
            }
            St::Store {
                place: Place::Elem(base, _),
                value,
                ..
            } if place_root(base).is_some_and(|a| arrays.contains(&a)) => {
                link(rel, place_root(base).into_iter().chain(val(value)));
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
                // A Bool a branch stores takes the branch's facts at the join
                // (`State::join`): `a && b && c` keeps `b`'s for a check or an
                // obligation.
                if goals != Goals::Results && val(cond).is_some_and(|c| rel[c.index()]) {
                    let mut set = BTreeSet::new();
                    stores(std::slice::from_ref(s), &mut set);
                    set.retain(|n| body.names[n.index()].ty == Type::Bool);
                    set.iter().for_each(|n| rel[n.index()] = true);
                }
                mark(body, decls, then, rel, goals, linked, arrays);
                mark(body, decls, els, rel, goals, linked, arrays);
            }
            St::Loop { body: l, .. } | St::Block { body: l, .. } => {
                // A loop bound by a literal counts what it stores
                // ([`Walk::in_step`]).
                if let Some((t, ..)) = bound_test(l).filter(|(.., b)| b.is_const()) {
                    let mut stored = BTreeSet::new();
                    stores(l, &mut stored);
                    let seq = |n: &Name| kind_of(&body.names[n.index()].ty, decls) == Kind::Seq;
                    if stored.iter().any(|n| rel[n.index()] && seq(n)) {
                        rel[t.index()] = true;
                    }
                }
                mark(body, decls, l, rel, goals, linked, arrays)
            }
            St::Switch { on, arms, .. } => {
                for a in arms {
                    if let Some(b) = some_binder(body, decls, on, a) {
                        link(rel, [b].into_iter().chain(val(on)));
                    }
                    mark(body, decls, &a.body, rel, goals, linked, arrays)
                }
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
        Guard::Range(s, a, b) => [s, a, b].into_iter().for_each(val),
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

/// Per name or field of a record name the rows of `ss` store to, the path to
/// each such store: the index of each enclosing row, and of the branch or arm
/// taken.
fn store_paths<'s>(
    ss: &'s [St],
    path: &mut Vec<u32>,
    out: &mut BTreeMap<(Name, Option<&'s str>), Vec<Vec<u32>>>,
) {
    for (i, s) in (0u32..).zip(ss) {
        path.push(i);
        let to = match s {
            St::Store {
                place: Place::Name(n),
                ..
            } => Some((*n, None)),
            St::Store {
                place: Place::Field(r, f),
                ..
            } => match **r {
                Place::Name(r) => Some((r, Some(f.as_str()))),
                _ => None,
            },
            _ => None,
        };
        if let Some(to) = to {
            out.entry(to)
                .or_default()
                .push(path[..path.len() - 1].to_vec());
        }
        match s {
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

/// `a - b` and its negation for each two fields of one record that `entry`
/// defines equally long, when the loop writes the record: a loop that grows
/// both in step keeps them equal. [`Walk::settle`] keeps a candidate only
/// where the entry and every turn prove it.
fn equal_fields(entry: &State, written: &BTreeSet<Name>) -> Vec<Lin> {
    let cols = || {
        (entry.defs.iter()).filter(|(t, _)| {
            matches!(t, Term::Col(..)) && t.name().is_some_and(|n| written.contains(&n))
        })
    };
    let mut out = Vec::new();
    for (i, (a, va)) in cols().enumerate() {
        let same = |(b, vb): &(&Term, &Lin)| b.name() == a.name() && *vb == va;
        for (b, _) in cols().skip(i + 1).filter(same) {
            let d = Lin::of(*a).sub(&Lin::of(*b));
            out.extend(d.as_ref().and_then(|d| d.scale(-1)));
            out.extend(d);
        }
    }
    out
}

/// The value the `let` of `n` in `ss` binds.
fn let_of(ss: &[St], n: Name) -> Option<&Rhs> {
    ss.iter().find_map(|s| match s {
        St::Let(m, rhs) if *m == n => Some(rhs),
        St::If { then, els, .. } => let_of(then, n).or_else(|| let_of(els, n)),
        St::Loop { body, .. } | St::Block { body, .. } => let_of(body, n),
        St::Switch { arms, .. } => arms.iter().find_map(|a| let_of(&a.body, n)),
        _ => None,
    })
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
