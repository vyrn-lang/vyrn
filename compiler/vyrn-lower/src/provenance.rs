//! Where each position of a function's array result comes from, for `vyrn
//! why --provenance`: the class of the backward map from the result's
//! positions to the positions of each array parameter, the positions the body
//! writes from elsewhere, and what happened to the values. It accepts and
//! refuses nothing.
//!
//! An abstract interpretation of one core body ([`of`]). Per array it tracks
//! the parameters its elements come from, per parameter the monotonicity of
//! the indexes pushed and the last of them, and for in-place moves the one
//! position whose content is duplicated and the names that hold the content
//! it displaced (the hole). Indexes are linear forms over the parameters, the
//! parameters' lengths and one term per loop counter, the counter's value at
//! the head of the current turn. Conditions are ignored, so a class holds for
//! every answer a predicate or comparator gives. A loop runs to a fixpoint of
//! at most [`ROUNDS`] turns; past it every array the loop sees is unknown.

use std::collections::{BTreeMap, BTreeSet};

use vyrn_frontend::ast::{BinOp, Capability, FnId, Program, Type, UnOp};
use vyrn_frontend::core::{Arg, Body, Ctor, Lit, Name, Op, Place, Rhs, St, Val};
use vyrn_frontend::prelude::{self, ShapeMap};

use crate::facts::{Lin, Term};
use crate::World;

/// The turns a loop runs before the analysis gives up on it.
const ROUNDS: usize = 8;

/// The name a `do` row's result binds: none.
const DISCARD: Name = Name(u32::MAX);

/// The class of the backward map from the result's positions that read one
/// parameter to that parameter's positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Identity,
    /// Positions 0 to k-1, in order.
    Prefix,
    /// Positions k to the last, in order.
    Suffix,
    /// Strictly increasing.
    Subsequence,
    /// Weakly increasing: a position may repeat.
    Stutter,
    /// Every position, last first.
    Reverse,
    /// Strictly decreasing.
    ReverseSubsequence,
    /// Every position once, in any order.
    Permutation,
    /// No position twice, in any order.
    Injection,
    /// Any position, any number of times.
    Gather,
}

/// Where the body writes positions whose values come from no parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Written {
    None,
    /// Only after every position read from a parameter.
    End,
    Inside,
}

/// What happened to the values a position reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Values {
    /// The element itself, moved or structurally copied.
    Same,
    /// The element rebuilt with these fields changed.
    Fields(BTreeSet<String>),
    /// A function of the element.
    Transformed,
}

impl Values {
    fn join(&self, o: &Values) -> Values {
        match (self, o) {
            (Values::Transformed, _) | (_, Values::Transformed) => Values::Transformed,
            (Values::Fields(a), Values::Fields(b)) => Values::Fields(a.union(b).cloned().collect()),
            (Values::Fields(a), Values::Same) | (Values::Same, Values::Fields(a)) => {
                Values::Fields(a.clone())
            }
            (Values::Same, Values::Same) => Values::Same,
        }
    }
}

/// How two or more parameters' positions sit in the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// All of one parameter's positions, then all of the next one's.
    Concat,
    /// Interleaved, each parameter's in its own class.
    Shuffle,
}

/// What a body's array result is made of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Provenance {
    /// The analysis cannot say; the reason, in a few words.
    Unknown(&'static str),
    /// No position reads a parameter.
    Fresh,
    Of {
        /// Each parameter some position reads, in parameter order, and its class.
        sources: Vec<(Name, Class)>,
        /// With two or more sources.
        layout: Option<Layout>,
        written: Written,
        values: Values,
    },
}

impl Provenance {
    /// The words of the analytic report's census for this provenance, such as
    /// `identity+fresh(end) pointwise`.
    pub fn label(&self) -> String {
        let (sources, layout, written, values) = match self {
            Provenance::Unknown(_) => return "unknown".into(),
            Provenance::Fresh => return "fresh".into(),
            Provenance::Of {
                sources,
                layout,
                written,
                values,
            } => (sources, layout, written, values),
        };
        let mut out = match (layout, sources.as_slice()) {
            (Some(Layout::Concat), _) => "concat".to_string(),
            (Some(Layout::Shuffle), _) => "shuffle".to_string(),
            (None, [(_, c)]) => c.word().to_string(),
            (None, _) => "unknown".to_string(),
        };
        match written {
            Written::None => {}
            Written::End => out.push_str("+fresh(end)"),
            Written::Inside => out.push_str("+fresh(inside)"),
        }
        match values {
            Values::Same => {}
            Values::Transformed => out.push_str(" pointwise"),
            Values::Fields(fs) => {
                let fs: Vec<&str> = fs.iter().map(String::as_str).collect();
                out.push_str(&format!(" field-update({})", fs.join(",")));
            }
        }
        out
    }

    /// [`Provenance::label`] with the parameters each class reads, named by
    /// `name`, or the reason an unknown gives: the line `vyrn why
    /// --provenance` prints.
    pub fn text(&self, name: &dyn Fn(Name) -> String) -> String {
        match self {
            Provenance::Unknown(why) => format!("unknown: {why}"),
            Provenance::Fresh => self.label(),
            Provenance::Of {
                sources, layout, ..
            } => {
                let from: Vec<String> = (sources.iter())
                    .map(|(n, c)| match layout {
                        Some(_) => format!("`{}` ({})", name(*n), c.word()),
                        None => format!("`{}`", name(*n)),
                    })
                    .collect();
                format!("{}, from {}", self.label(), from.join(", "))
            }
        }
    }
}

impl Class {
    fn word(self) -> &'static str {
        match self {
            Class::Identity => "identity",
            Class::Prefix => "prefix",
            Class::Suffix => "suffix",
            Class::Subsequence => "subsequence",
            Class::Stutter => "stutter",
            Class::Reverse => "reverse",
            Class::ReverseSubsequence => "reverse-subsequence",
            Class::Permutation => "permutation",
            Class::Injection => "injection",
            Class::Gather => "gather",
        }
    }
}

/// The direction of the indexes pushed from one source so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mono {
    /// One push.
    One,
    Inc,
    WInc,
    Dec,
    WDec,
    None,
}

impl Mono {
    fn join(self, o: Mono) -> Mono {
        use Mono::*;
        match (self, o) {
            (a, b) if a == b => a,
            (One, x) | (x, One) if x != None => x,
            (Inc, WInc) | (WInc, Inc) => WInc,
            (Dec, WDec) | (WDec, Dec) => WDec,
            _ => None,
        }
    }

    /// After a push `dd` past the front.
    fn step(self, dd: i64) -> Mono {
        use Mono::*;
        match (self, dd.signum()) {
            (One | Inc, 1) => Inc,
            (WInc, 1) | (One | Inc | WInc, 0) => WInc,
            (One | Dec, -1) => Dec,
            (WDec, -1) | (Dec | WDec, 0) => WDec,
            _ => None,
        }
    }
}

/// One source's positions in an array.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Src {
    mono: Mono,
    /// The extreme index pushed, in the direction of travel; `None` once
    /// unknown.
    front: Option<Lin>,
    /// The first index pushed; `None` where two paths disagree.
    first: Option<Lin>,
    /// Every push sat next to the one before it.
    dense: bool,
    /// The last push was the source's last position, or its first for a
    /// decreasing run.
    ends: bool,
}

impl Src {
    fn join(&self, o: &Src) -> Src {
        let mono = self.mono.join(o.mono);
        let front = match (&self.front, &o.front) {
            (Some(a), Some(b)) if a == b => Some(a.clone()),
            (Some(a), Some(b)) => match b.sub(a).filter(Lin::is_const).map(|d| d.c) {
                Some(d) if matches!(mono, Mono::Inc | Mono::WInc) => {
                    Some(if d > 0 { b } else { a }.clone())
                }
                Some(d) if matches!(mono, Mono::Dec | Mono::WDec) => {
                    Some(if d < 0 { b } else { a }.clone())
                }
                _ => None,
            },
            _ => None,
        };
        Src {
            mono,
            dense: self.dense && o.dense && self.front == o.front,
            front,
            first: Some(&self.first)
                .filter(|f| **f == o.first)
                .cloned()
                .flatten(),
            ends: self.ends && o.ends,
        }
    }
}

/// An array the body builds or holds.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Arr {
    /// The name that made the array: its identity as it moves between names.
    id: Name,
    /// The parameter whose unchanged value this is.
    pristine: Option<Name>,
    srcs: BTreeMap<Name, Src>,
    /// Sources another source's push followed.
    closed: BTreeSet<Name>,
    interleaved: bool,
    written: Written,
    values: Values,
    /// An element was stored in place.
    moved: bool,
    /// No source position sits at two positions.
    inj: bool,
    /// A position reads a source position no index order states.
    gather: bool,
    /// What the array holds is past the analysis.
    unknown: Option<&'static str>,
    /// The position whose content another position duplicates, and the names
    /// that hold the content that store displaced.
    hole: Option<(Lin, BTreeSet<Name>)>,
}

impl Arr {
    fn new(id: Name) -> Arr {
        Arr {
            id,
            pristine: None,
            srcs: BTreeMap::new(),
            closed: BTreeSet::new(),
            interleaved: false,
            written: Written::None,
            values: Values::Same,
            moved: false,
            inj: true,
            gather: false,
            unknown: None,
            hole: None,
        }
    }

    /// The unchanged value of parameter `p`.
    fn whole(id: Name, p: Name) -> Arr {
        let src = Src {
            mono: Mono::Inc,
            front: None,
            first: Some(Lin::k(0)),
            dense: true,
            ends: true,
        };
        Arr {
            pristine: Some(p),
            srcs: BTreeMap::from([(p, src)]),
            ..Arr::new(id)
        }
    }

    fn lost(&mut self, why: &'static str) {
        self.unknown.get_or_insert(why);
    }

    fn join(&self, o: &Arr) -> Arr {
        if self.id != o.id {
            let mut a = Arr::new(self.id);
            a.lost("two paths hold different arrays");
            return a;
        }
        let mut srcs = BTreeMap::new();
        for k in self.srcs.keys().chain(o.srcs.keys()) {
            let s = match (self.srcs.get(k), o.srcs.get(k)) {
                (Some(a), Some(b)) => a.join(b),
                (Some(a), None) | (None, Some(a)) => Src {
                    first: None,
                    ends: false,
                    ..a.clone()
                },
                (None, None) => continue,
            };
            srcs.insert(*k, s);
        }
        let hole = Some(&self.hole)
            .filter(|h| **h == o.hole)
            .cloned()
            .flatten();
        Arr {
            id: self.id,
            pristine: self.pristine.filter(|p| o.pristine == Some(*p)),
            srcs,
            closed: self.closed.union(&o.closed).copied().collect(),
            interleaved: self.interleaved || o.interleaved,
            written: self.written.max(o.written),
            values: self.values.join(&o.values),
            moved: self.moved || o.moved,
            inj: self.inj && o.inj && self.hole == o.hole,
            gather: self.gather || o.gather,
            unknown: self.unknown.or(o.unknown),
            hole,
        }
    }

    /// Records a push of `e`; `turn` counts pushes per source this loop turn.
    fn push(&mut self, e: &Elem, turn: &mut BTreeMap<(Name, Name), Option<u32>>) {
        self.pristine = None;
        let e = match e {
            Elem::Own { arr, .. } if *arr == self.id => {
                self.moved = true;
                self.inj = false;
                return;
            }
            e => e.source(),
        };
        let (src, at, values) = match &e {
            Elem::Of { src, at, values } => (*src, at, values),
            Elem::Own { .. } => return self.lost("it holds another built array's elements"),
            Elem::Field { .. } | Elem::Fresh => {
                if self.written == Written::None {
                    self.written = Written::End;
                }
                return;
            }
        };
        if self.written != Written::None {
            self.written = Written::Inside;
        }
        self.values = self.values.join(values);
        if self.closed.contains(&src) {
            self.interleaved = true;
        }
        self.closed.extend(self.srcs.keys().filter(|k| **k != src));
        let n = turn.entry((self.id, src)).or_insert(Some(0));
        *n = n.map(|n| n + 1);
        let Some(at) = at else {
            self.gather = true;
            self.inj = false;
            let s = self.srcs.entry(src).or_insert_with(|| Src {
                mono: Mono::None,
                front: None,
                first: None,
                dense: false,
                ends: false,
            });
            s.mono = Mono::None;
            return;
        };
        let Some(s) = self.srcs.get_mut(&src) else {
            self.srcs.insert(
                src,
                Src {
                    mono: Mono::One,
                    front: Some(at.clone()),
                    first: Some(at.clone()),
                    dense: true,
                    ends: false,
                },
            );
            return;
        };
        s.ends = false;
        let dd = s
            .front
            .as_ref()
            .and_then(|f| at.sub(f))
            .filter(Lin::is_const);
        let Some(dd) = dd.map(|d| d.c) else {
            s.mono = Mono::None;
            s.dense = false;
            s.front = None;
            self.inj = false;
            return;
        };
        if dd == 0 {
            self.inj = false;
        }
        s.mono = s.mono.step(dd);
        s.dense &= match s.mono {
            Mono::Inc => dd == 1,
            Mono::Dec => dd == -1,
            _ => false,
        };
        let forward = match s.mono {
            Mono::Inc | Mono::WInc => dd >= 0,
            Mono::Dec | Mono::WDec => dd <= 0,
            _ => false,
        };
        s.front = match s.mono {
            Mono::None => None,
            _ if forward => Some(at.clone()),
            _ => s.front.take(),
        };
    }

    /// Records the store of `e`, held by `name` where a name holds it, at
    /// index `at`. `holders` are the names that hold this array's element at
    /// `at` before the store.
    fn store(&mut self, at: Option<Lin>, e: &Elem, name: Option<Name>, holders: BTreeSet<Name>) {
        self.pristine = None;
        self.moved = true;
        let own = match e {
            Elem::Own { arr, at, .. } if *arr == self.id => Some(at.clone()),
            _ => None,
        };
        match (self.hole.take(), &at) {
            (Some((h, held)), Some(i)) if h == *i => {
                match (own, name) {
                    (_, Some(n)) if held.contains(&n) => {}
                    (Some(Some(j)), _) => self.hole = Some((j, held)),
                    _ => self.overwrite(e),
                }
                return;
            }
            (Some(_), _) => {
                self.inj = false;
            }
            (None, _) => {}
        }
        match (own, at) {
            (Some(Some(j)), Some(i)) if j == i => {}
            (Some(Some(j)), Some(_)) => {
                if holders.is_empty() {
                    self.inj = false;
                }
                self.hole = Some((j, holders));
            }
            _ => self.overwrite(e),
        }
    }

    /// A store of `e` that moves no content of this array.
    fn overwrite(&mut self, e: &Elem) {
        self.inj = false;
        match e.source() {
            Elem::Of { values, .. } => {
                self.gather = true;
                self.values = self.values.join(&values);
            }
            Elem::Own { .. } => self.lost("it stores another array's elements in place"),
            Elem::Field { .. } | Elem::Fresh => self.written = Written::Inside,
        }
    }

    fn classify(&self) -> Provenance {
        if let Some(why) = self.unknown {
            return Provenance::Unknown(why);
        }
        if self.hole.is_some() {
            return Provenance::Unknown("a moved element is not put back");
        }
        if self.srcs.is_empty() {
            return Provenance::Fresh;
        }
        let full = |s: &Src| {
            matches!(s.mono, Mono::Inc | Mono::One)
                && s.dense
                && s.ends
                && s.first == Some(Lin::k(0))
        };
        let class = |p: Name, s: &Src| -> Option<Class> {
            if self.gather {
                return Some(Class::Gather);
            }
            if self.moved {
                return match self.inj {
                    true if full(s) => Some(Class::Permutation),
                    true => Some(Class::Injection),
                    false => None,
                };
            }
            let last = Lin::of(Term::Len(p)).plus(-1);
            Some(match s.mono {
                Mono::Inc | Mono::One if s.dense => match (s.first == Some(Lin::k(0)), s.ends) {
                    (true, true) => Class::Identity,
                    (true, false) => Class::Prefix,
                    (false, true) => Class::Suffix,
                    (false, false) => Class::Subsequence,
                },
                Mono::Inc | Mono::One => Class::Subsequence,
                Mono::WInc => Class::Stutter,
                Mono::Dec if s.dense && s.ends && s.first == last => Class::Reverse,
                Mono::Dec => Class::ReverseSubsequence,
                Mono::WDec | Mono::None => return None,
            })
        };
        let mut sources = Vec::new();
        for (p, s) in &self.srcs {
            match class(*p, s) {
                Some(c) => sources.push((*p, c)),
                None => return Provenance::Unknown("no order relates its indexes"),
            }
        }
        let layout = match sources.len() {
            1 => None,
            _ if self.moved || self.gather => return Provenance::Unknown("it mixes moved sources"),
            _ if self.interleaved => Some(Layout::Shuffle),
            _ => Some(Layout::Concat),
        };
        Provenance::Of {
            sources,
            layout,
            written: self.written,
            values: self.values.clone(),
        }
    }
}

/// A value a name holds that is, or comes from, an array element.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Elem {
    /// Position `at` of parameter `src`; `at` is `None` where the index is
    /// data.
    Of {
        src: Name,
        at: Option<Lin>,
        values: Values,
    },
    /// Field `field` of an unchanged such element.
    Field {
        src: Name,
        at: Option<Lin>,
        field: String,
    },
    /// The element at `at` of the array made at `arr` when it was read, and
    /// the parameter whose position that is where the array was unchanged.
    Own {
        arr: Name,
        at: Option<Lin>,
        of: Option<Name>,
    },
    Fresh,
}

impl Elem {
    /// A function of this value.
    fn transformed(&self) -> Elem {
        match self.source() {
            Elem::Of { src, at, .. } | Elem::Field { src, at, .. } => Elem::Of {
                src,
                at,
                values: Values::Transformed,
            },
            _ => Elem::Fresh,
        }
    }

    /// The element as a parameter's position, where it is one.
    fn source(&self) -> Elem {
        match self {
            Elem::Own {
                at, of: Some(p), ..
            } => Elem::Of {
                src: *p,
                at: at.clone(),
                values: Values::Same,
            },
            e => e.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct State {
    ints: BTreeMap<Name, Lin>,
    elems: BTreeMap<Name, Elem>,
    /// The arrays by the name that holds each.
    arrays: BTreeMap<Name, Arr>,
    /// A borrow of an array, to the name that holds it.
    alias: BTreeMap<Name, Name>,
    /// Pushes per array and source this loop turn: the count on every path,
    /// `None` where paths differ.
    turn: BTreeMap<(Name, Name), Option<u32>>,
}

impl State {
    fn join(&self, o: &State) -> State {
        fn common<K: Ord + Copy, V: Clone + PartialEq>(
            a: &BTreeMap<K, V>,
            b: &BTreeMap<K, V>,
        ) -> BTreeMap<K, V> {
            (a.iter())
                .filter(|(k, v)| b.get(k) == Some(v))
                .map(|(k, v)| (*k, v.clone()))
                .collect()
        }
        let mut arrays = self.arrays.clone();
        for (n, b) in &o.arrays {
            let a = arrays.get(n).map_or_else(|| b.clone(), |a| a.join(b));
            arrays.insert(*n, a);
        }
        let mut turn = BTreeMap::new();
        for k in self.turn.keys().chain(o.turn.keys()) {
            let (a, b) = (self.count(k), o.count(k));
            turn.insert(*k, Some(a).filter(|a| *a == b).flatten());
        }
        State {
            ints: common(&self.ints, &o.ints),
            elems: common(&self.elems, &o.elems),
            arrays,
            alias: common(&self.alias, &o.alias),
            turn,
        }
    }

    fn count(&self, k: &(Name, Name)) -> Option<u32> {
        self.turn.get(k).copied().unwrap_or(Some(0))
    }

    /// The name that holds the array `n` names.
    fn root(&self, n: Name) -> Name {
        self.alias.get(&n).copied().unwrap_or(n)
    }

    /// Rewrites every linear form by `by`, which `None` forgets.
    fn rewrite(&mut self, by: &dyn Fn(&Lin) -> Option<Lin>) {
        self.ints = (self.ints.iter())
            .filter_map(|(n, l)| Some((*n, by(l)?)))
            .collect();
        for e in self.elems.values_mut() {
            if let Elem::Of { at, .. } | Elem::Field { at, .. } | Elem::Own { at, .. } = e {
                *at = at.as_ref().and_then(by);
            }
        }
        for a in self.arrays.values_mut() {
            for s in a.srcs.values_mut() {
                s.front = s.front.as_ref().and_then(by);
                s.first = s.first.as_ref().and_then(by);
            }
            if let Some((h, held)) = a.hole.take() {
                match by(&h) {
                    Some(h) => a.hole = Some((h, held)),
                    None => a.inj = false,
                }
            }
        }
    }

    /// Forgets the values of `names`, each bound afresh every loop turn.
    fn unbind(&mut self, names: &BTreeSet<Name>) {
        self.ints.retain(|n, _| !names.contains(n));
        self.elems.retain(|n, _| !names.contains(n));
        self.arrays.retain(|n, _| !names.contains(n));
        self.alias.retain(|n, _| !names.contains(n));
        for a in self.arrays.values_mut() {
            if let Some((_, held)) = &mut a.hole {
                held.retain(|n| !names.contains(n));
            }
        }
    }
}

/// A loop's head test: the counter it reads and what bounds it.
struct Head {
    counter: Name,
    bound: Bound,
}

enum Bound {
    /// `counter < e`, with `e` the length of this parameter where it is one.
    Below(Option<Name>),
    /// `counter >= 0`.
    AtLeastZero,
}

/// A run over one body.
struct Walk<'b> {
    body: &'b Body,
    /// The parameters no row writes: their values and lengths are terms.
    consts: BTreeSet<Name>,
    /// The names a `match` arm binds to a payload.
    payloads: BTreeSet<Name>,
    returns: Vec<Provenance>,
    /// The next loop counter's term.
    ghosts: u32,
    breaks: Vec<State>,
    continues: Vec<State>,
}

/// The provenance of each function the root file declares that returns an
/// array, with the frame it read, in function-table order
/// ([`crate::insight::root_frames`]).
pub fn table<'w>(program: &Program, world: &'w World) -> Vec<(FnId, &'w Body, Provenance)> {
    (crate::insight::root_frames(program, world).into_iter())
        .filter(|(_, b)| !b.name.contains("@lambda"))
        .filter_map(|(id, b)| Some((id, b, of(b)?)))
        .collect()
}

/// The provenance of `body`'s array result, `None` when it returns no array.
pub fn of(body: &Body) -> Option<Provenance> {
    let returns_array = (rows_of(&body.stmts).iter()).any(|s| match s {
        St::Return {
            value: Some(Val::Name(n)),
            ..
        } => is_array(&body.names[n.index()].ty),
        _ => false,
    });
    if !returns_array {
        return None;
    }
    let mut written = BTreeSet::new();
    writes(&body.stmts, &mut written);
    let consts: BTreeSet<Name> = (body.params.iter().copied())
        .filter(|p| !written.contains(p))
        .collect();
    let mut st = State::default();
    for p in &body.params {
        if is_array(&body.names[p.index()].ty) {
            st.arrays.insert(*p, Arr::whole(*p, *p));
        }
    }
    let payloads = (rows_of(&body.stmts).into_iter())
        .flat_map(|s| match s {
            St::Switch { arms, .. } => arms.iter().flat_map(|a| a.binds.clone()).collect(),
            _ => Vec::new(),
        })
        .collect();
    let mut w = Walk {
        body,
        consts,
        payloads,
        returns: Vec::new(),
        ghosts: 0,
        breaks: Vec::new(),
        continues: Vec::new(),
    };
    w.block(&body.stmts, Some(st));
    let mut out: Option<Provenance> = None;
    for r in w.returns {
        out = Some(match out {
            None => r,
            Some(p) if p == r => p,
            Some(_) => Provenance::Unknown("two returns differ"),
        });
    }
    out
}

fn is_array(ty: &Type) -> bool {
    matches!(ty, Type::Array(_))
}

/// Every row of `ss`, nested rows included.
fn rows_of(ss: &[St]) -> Vec<&St> {
    vyrn_frontend::core::rows(ss).map(|(s, _)| s).collect()
}

/// The names a row of `ss` stores into or hands a call to write.
fn writes(ss: &[St], out: &mut BTreeSet<Name>) {
    for s in rows_of(ss) {
        let rhs = match s {
            St::Store { place, .. } => {
                out.insert(place_root(place));
                continue;
            }
            St::Let(_, rhs) | St::Do { rhs, .. } => rhs,
            _ => continue,
        };
        if let Rhs::Call { args, .. } = rhs {
            for (a, c) in args {
                if let (Arg::Place(p), Capability::Modify) = (a, c) {
                    out.insert(place_root(p));
                }
            }
        }
    }
}

fn place_root(p: &Place) -> Name {
    match p {
        Place::Name(n) => *n,
        Place::Field(b, _) | Place::Elem(b, _) | Place::Key(b, _) => place_root(b),
        Place::Global(_) => DISCARD,
    }
}

fn breaks_in(ss: &[St]) -> usize {
    let mut n = 0;
    for s in ss {
        n += match s {
            St::Break { .. } => 1,
            St::If { then, els, .. } => breaks_in(then) + breaks_in(els),
            St::Block { body, .. } => breaks_in(body),
            St::Switch { arms, .. } => arms.iter().map(|a| breaks_in(&a.body)).sum(),
            _ => 0,
        };
    }
    n
}

impl Walk<'_> {
    /// Runs `ss` from `st`; `None` where no path falls through.
    fn block(&mut self, ss: &[St], mut st: Option<State>) -> Option<State> {
        for s in ss {
            let cur = st.as_mut()?;
            match s {
                St::Let(n, rhs) => self.let_(cur, *n, rhs),
                St::Do { rhs, .. } => {
                    self.let_(cur, DISCARD, rhs);
                    cur.unbind(&BTreeSet::from([DISCARD]));
                }
                St::Store { place, value, .. } => self.store(cur, place, value),
                St::If { then, els, .. } => {
                    let a = self.block(then, st.clone());
                    let b = self.block(els, st.take());
                    st = join(a, b);
                }
                St::Block { body, .. } => st = self.block(body, st.take()),
                St::Switch { arms, .. } => {
                    let entry = st.take();
                    for arm in arms {
                        let out = self.block(&arm.body, entry.clone());
                        st = join(st, out);
                    }
                }
                St::Loop { body, .. } => st = self.looped(body, st.take()?),
                St::Break { .. } => {
                    self.breaks.extend(st.take());
                }
                St::Continue { .. } => {
                    self.continues.extend(st.take());
                }
                St::Return { value, .. } => {
                    let p = match value {
                        Some(Val::Name(n)) => match cur.arrays.get(&cur.root(*n)) {
                            Some(a) => a.classify(),
                            None => {
                                Provenance::Unknown("it returns an array the body did not build")
                            }
                        },
                        _ => Provenance::Unknown("it returns no name"),
                    };
                    self.returns.push(p);
                    st = None;
                }
                St::Trap => st = None,
                St::Drop(..) | St::Row { .. } | St::Check(_) => {}
            }
        }
        st
    }

    fn looped(&mut self, body: &[St], entry: State) -> Option<State> {
        let mut changed = BTreeSet::new();
        writes(body, &mut changed);
        let bound_here: BTreeSet<Name> = (rows_of(body).iter())
            .filter_map(|s| match s {
                St::Let(n, _) => Some(*n),
                _ => None,
            })
            .collect();
        let counters: Vec<Name> = (changed.iter().copied())
            .filter(|n| !bound_here.contains(n) && entry.ints.contains_key(n))
            .collect();
        let ghosts: BTreeMap<Name, Term> = (counters.iter())
            .map(|n| {
                self.ghosts += 1;
                (*n, Term::Ghost(self.ghosts, false))
            })
            .collect();
        let start: BTreeMap<Name, Lin> = (counters.iter())
            .filter_map(|n| Some((*n, entry.ints.get(n)?.clone())))
            .collect();
        let outer_turn = entry.turn.clone();
        let mut head = entry.clone();
        head.unbind(&bound_here);
        head.turn.clear();
        for (n, g) in &ghosts {
            head.ints.insert(*n, Lin::of(*g));
        }
        let saved = (
            std::mem::take(&mut self.breaks),
            std::mem::take(&mut self.continues),
        );
        let test = head_test(body, self, &head);
        let mut stable = false;
        let mut back = None;
        for _ in 0..ROUNDS {
            self.breaks.clear();
            self.continues.clear();
            let end = self.block(body, Some(head.clone()));
            let mut b = std::mem::take(&mut self.continues)
                .into_iter()
                .fold(end, |a, s| join(a, Some(s)));
            back = b.clone();
            let next = match &mut b {
                Some(b) => {
                    for (n, g) in &ghosts {
                        let step = (b.ints.get(n))
                            .and_then(|l| l.sub(&Lin::of(*g)))
                            .filter(Lin::is_const);
                        let g = *g;
                        match step {
                            Some(d) => {
                                let by = Lin::of(g).plus(-d.c);
                                b.rewrite(&|l| match by.as_ref() {
                                    Some(by) => l.subst(g, by),
                                    None => None,
                                });
                            }
                            None => b.rewrite(&|l| Some(l.clone()).filter(|l| l.coef(g) == 0)),
                        }
                    }
                    b.unbind(&bound_here);
                    for (n, g) in &ghosts {
                        b.ints.insert(*n, Lin::of(*g));
                    }
                    b.turn.clear();
                    head.join(b)
                }
                None => head.clone(),
            };
            if next == head {
                stable = true;
                break;
            }
            head = next;
        }
        let mut exits = std::mem::take(&mut self.breaks);
        (self.breaks, self.continues) = saved;
        if !stable {
            for s in &mut exits {
                for a in s.arrays.values_mut() {
                    a.lost("a loop over it did not settle");
                }
            }
        }
        // A counted loop: its head test is its only exit, and every turn pushes
        // one element of a source at the counter and steps the counter by one.
        let counted: Vec<(Name, Name, bool, bool)> = match (&test, &back) {
            (Some(t), Some(b)) if breaks_in(body) == 1 => {
                let g = ghosts.get(&t.counter).copied();
                let step = g.and_then(|g| b.ints.get(&t.counter)?.sub(&Lin::of(g)));
                let up = match (step.map(|d| d.c), &t.bound) {
                    (Some(1), Bound::Below(_)) => Some(true),
                    (Some(-1), Bound::AtLeastZero) => Some(false),
                    _ => None,
                };
                match (g, up) {
                    (Some(g), Some(up)) => (b.turn.iter())
                        .filter(|(_, n)| **n == Some(1))
                        .filter_map(|((id, src), _)| {
                            let arr = b.arrays.values().find(|a| a.id == *id)?;
                            let s = arr.srcs.get(src)?;
                            (s.front == Some(Lin::of(g))).then_some((*id, *src, up, false))
                        })
                        .filter(|(id, src, ..)| {
                            !(entry.arrays.values())
                                .any(|a| a.id == *id && a.srcs.contains_key(src))
                        })
                        .map(|(id, src, up, _)| {
                            let ends = match &t.bound {
                                Bound::Below(p) => *p == Some(src),
                                Bound::AtLeastZero => true,
                            };
                            (id, src, up, ends)
                        })
                        .collect(),
                    _ => Vec::new(),
                }
            }
            _ => Vec::new(),
        };
        let mut out = exits.into_iter().fold(None, |a, s| join(a, Some(s)));
        if let Some(o) = &mut out {
            // A loop's counter terms mean nothing past its exit.
            for g in ghosts.values() {
                let g = *g;
                o.rewrite(&|l| Some(l.clone()).filter(|l| l.coef(g) == 0));
            }
            let first = (test.as_ref()).and_then(|t| start.get(&t.counter)).cloned();
            for (id, src, up, ends) in counted {
                for a in o.arrays.values_mut().filter(|a| a.id == id) {
                    let Some(s) = a.srcs.get_mut(&src) else {
                        continue;
                    };
                    *s = Src {
                        mono: if up { Mono::Inc } else { Mono::Dec },
                        front: None,
                        first: first.clone(),
                        dense: true,
                        ends,
                    };
                }
            }
            o.turn = outer_turn;
            for k in (head.turn.keys()).chain(back.iter().flat_map(|b| b.turn.keys())) {
                o.turn.insert(*k, None);
            }
        }
        out
    }

    fn let_(&mut self, st: &mut State, n: Name, rhs: &Rhs) {
        self.effects(st, rhs);
        if let Some(l) = self.int(st, rhs) {
            st.ints.insert(n, l);
        }
        match rhs {
            Rhs::Val(Val::Name(m)) | Rhs::Take(Place::Name(m)) => {
                if let Some(r) = st.alias.get(m).copied() {
                    st.alias.insert(n, r);
                    return;
                }
                let m = *m;
                if let Some(a) = st.arrays.get(&m).cloned() {
                    st.arrays.insert(n, a);
                }
                if let Some(e) = st.elems.get(&m).cloned() {
                    st.elems.insert(n, e);
                }
            }
            Rhs::Read(Place::Name(m)) => {
                let m = st.root(*m);
                if self.payloads.contains(&n) {
                    let e = st.elems.get(&m).map_or(Elem::Fresh, Elem::transformed);
                    st.elems.insert(n, e);
                } else if st.arrays.contains_key(&m) {
                    st.alias.insert(n, m);
                } else if let Some(e) = st.elems.get(&m).cloned() {
                    st.elems.insert(n, e);
                }
            }
            Rhs::Read(p) | Rhs::Take(p) => {
                let e = self.element(st, p);
                st.elems.insert(n, e);
            }
            Rhs::Make(Ctor::Array, parts) => {
                let mut a = Arr::new(n);
                for v in parts {
                    let e = value(st, v);
                    a.push(&e, &mut st.turn);
                }
                st.arrays.insert(n, a);
            }
            Rhs::Make(Ctor::Record(_, fields), parts) => {
                let e = rebuilt(st, fields, parts);
                st.elems.insert(n, e);
            }
            Rhs::Make(..) | Rhs::Val(Val::Lit(_)) => {}
            Rhs::Prim(_, vs, _) => {
                if let Some(e) = derived(vs.iter().map(|v| value(st, v))) {
                    st.elems.insert(n, e);
                }
            }
            Rhs::Call { callee, args, .. } => self.call(st, n, callee, args),
        }
    }

    fn call(&mut self, st: &mut State, n: Name, callee: &str, args: &[(Arg, Capability)]) {
        let arg = |st: &State, i: usize| -> Elem {
            match args.get(i) {
                Some((Arg::Val(v), _)) => value(st, v),
                Some((Arg::Place(Place::Name(m)), _)) => value(st, &Val::Name(*m)),
                _ => Elem::Fresh,
            }
        };
        let recv = match args.first() {
            Some((Arg::Val(Val::Name(m)) | Arg::Place(Place::Name(m)), c)) => {
                Some((st.root(*m), *c))
            }
            _ => None,
        };
        if callee == "@copy" {
            if let Some((m, _)) = recv {
                if let Some(a) = st.arrays.get(&m) {
                    let a = Arr { id: n, ..a.clone() };
                    st.arrays.insert(n, a);
                } else {
                    let e = arg(st, 0);
                    st.elems.insert(n, e);
                }
            }
            return;
        }
        let map = prelude::builtin(callee).map(|b| b.shape_map_at(args.len()));
        if let (Some(map), Some((m, cap))) = (map, recv) {
            if let Some(mut a) = st
                .arrays
                .get(&m)
                .cloned()
                .filter(|_| map != ShapeMap::Unknown)
            {
                match map {
                    ShapeMap::Same => {}
                    ShapeMap::Pushes(k) => {
                        let e = arg(st, k);
                        a.push(&e, &mut st.turn);
                    }
                    ShapeMap::Appends(k) => self.append(st, &mut a, args.get(k)),
                    ShapeMap::DropsLast => {
                        a.pristine = None;
                        a.srcs.values_mut().for_each(|s| s.ends = false);
                    }
                    ShapeMap::SwapsOut(_) => {
                        a.pristine = None;
                        a.moved = true;
                        a.srcs.values_mut().for_each(|s| s.ends = false);
                    }
                    ShapeMap::Empties => a = Arr::new(a.id),
                    ShapeMap::CopiesFrom(k) => {
                        a = match args.get(k) {
                            Some((Arg::Val(Val::Name(y)), _)) => {
                                match st.arrays.get(&st.root(*y)) {
                                    Some(b) => Arr {
                                        id: a.id,
                                        ..b.clone()
                                    },
                                    None => Arr::new(a.id),
                                }
                            }
                            _ => Arr::new(a.id),
                        };
                    }
                    ShapeMap::Extends | ShapeMap::Unknown => a.lost("a builtin changes it"),
                }
                match cap {
                    Capability::Modify => {
                        st.arrays.insert(m, a);
                    }
                    _ => {
                        st.arrays.insert(n, a);
                    }
                }
                return;
            }
        }
        let vs: Vec<Elem> = (0..args.len()).map(|i| arg(st, i)).collect();
        if let Some(e) = derived(vs.into_iter()) {
            st.elems.insert(n, e);
        }
    }

    /// `a.append(xs)`: every position of `xs`, in order, after `a`'s.
    fn append(&mut self, st: &mut State, a: &mut Arr, xs: Option<&(Arg, Capability)>) {
        let y = match xs {
            Some((Arg::Val(Val::Name(y)), _)) => st.root(*y),
            _ => return a.push(&Elem::Fresh, &mut st.turn),
        };
        match st.arrays.get(&y).and_then(|b| b.pristine) {
            Some(p) if !a.srcs.contains_key(&p) => {
                let at = Some(Lin::k(0));
                a.push(
                    &Elem::Of {
                        src: p,
                        at,
                        values: Values::Same,
                    },
                    &mut st.turn,
                );
                if let Some(s) = a.srcs.get_mut(&p) {
                    s.mono = Mono::Inc;
                    s.ends = true;
                    s.front = None;
                }
            }
            Some(_) => a.lost("it appends a source twice"),
            None if st.arrays.contains_key(&y) => a.lost("it appends a built array"),
            None => a.push(&Elem::Fresh, &mut st.turn),
        }
    }

    /// What a call or a row that writes does to the arrays it hands a call
    /// `modify`.
    fn effects(&mut self, st: &mut State, rhs: &Rhs) {
        let Rhs::Call { callee, args, .. } = rhs else {
            return;
        };
        let shapes = prelude::builtin(callee).is_some_and(|b| b.shape_map != ShapeMap::Unknown);
        for (i, (a, c)) in args.iter().enumerate() {
            if *c != Capability::Modify || (shapes && i == 0) {
                continue;
            }
            let root = match a {
                Arg::Place(p) => st.root(place_root(p)),
                Arg::Val(Val::Name(m)) => st.root(*m),
                Arg::Val(_) => continue,
            };
            if let Some(arr) = st.arrays.get_mut(&root) {
                arr.lost("a call may change it");
            }
        }
    }

    fn store(&mut self, st: &mut State, place: &Place, v: &Val) {
        match place {
            Place::Name(n) => {
                match self.int_of(st, v) {
                    Some(l) => st.ints.insert(*n, l),
                    None => st.ints.remove(n),
                };
                let e = value(st, v);
                st.elems.insert(*n, e);
                for a in st.arrays.values_mut() {
                    if let Some((_, held)) = &mut a.hole {
                        held.remove(n);
                    }
                }
                st.alias.remove(n);
                match v {
                    Val::Name(m) => match st.arrays.get(&st.root(*m)).cloned() {
                        Some(a) => st.arrays.insert(*n, a),
                        None => st.arrays.remove(n),
                    },
                    Val::Lit(_) => st.arrays.remove(n),
                };
            }
            Place::Elem(base, i) => {
                let Place::Name(b) = &**base else {
                    return self.touched(st, place);
                };
                let root = st.root(*b);
                let Some(id) = st.arrays.get(&root).map(|a| a.id) else {
                    return;
                };
                let at = self.int_of(st, i);
                let e = value(st, v);
                let mut holders = BTreeSet::new();
                for (n, held) in st.elems.iter_mut() {
                    let Elem::Own { arr, at: h, .. } = held else {
                        continue;
                    };
                    if *arr != id {
                        continue;
                    }
                    let same = match (h.as_ref(), at.as_ref()) {
                        (Some(h), Some(a)) => h.sub(a).filter(Lin::is_const).map(|d| d.c == 0),
                        _ => None,
                    };
                    match same {
                        Some(true) => {
                            holders.insert(*n);
                            *held = Elem::Fresh;
                        }
                        Some(false) => {}
                        None => *held = Elem::Fresh,
                    }
                }
                let name = match v {
                    Val::Name(m) => Some(*m),
                    Val::Lit(_) => None,
                };
                if let Some(a) = st.arrays.get_mut(&root) {
                    a.store(at, &e, name, holders);
                }
            }
            _ => self.touched(st, place),
        }
    }

    /// A store into a part of an element, a field or a key.
    fn touched(&mut self, st: &mut State, place: &Place) {
        let root = st.root(place_root(place));
        let field = match place {
            Place::Field(b, f) if matches!(&**b, Place::Elem(..)) => Some(f.clone()),
            _ => None,
        };
        if let Some(a) = st.arrays.get_mut(&root) {
            a.pristine = None;
            a.values = a.values.join(&match field {
                Some(f) => Values::Fields(BTreeSet::from([f])),
                None => Values::Transformed,
            });
        }
    }

    /// The element a place read yields.
    fn element(&self, st: &State, p: &Place) -> Elem {
        match p {
            Place::Elem(base, i) => {
                let Place::Name(b) = &**base else {
                    return Elem::Fresh;
                };
                let at = self.int_of(st, i);
                match st.arrays.get(&st.root(*b)) {
                    Some(a) => Elem::Own {
                        arr: a.id,
                        at,
                        of: a.pristine,
                    },
                    None => Elem::Fresh,
                }
            }
            Place::Field(base, f) => {
                let of = match &**base {
                    Place::Name(e) => st.elems.get(e).cloned(),
                    Place::Elem(..) => Some(self.element(st, base)),
                    _ => None,
                };
                match of.map(|e| e.source()) {
                    Some(Elem::Of {
                        src,
                        at,
                        values: Values::Same,
                    }) => Elem::Field {
                        src,
                        at,
                        field: f.clone(),
                    },
                    Some(e) => e.transformed(),
                    None => Elem::Fresh,
                }
            }
            _ => Elem::Fresh,
        }
    }

    /// The linear form of an integer `rhs`, where it has one.
    fn int(&self, st: &State, rhs: &Rhs) -> Option<Lin> {
        match rhs {
            Rhs::Val(v) => self.int_of(st, v),
            Rhs::Read(Place::Field(b, f)) if f == "length" => match &**b {
                Place::Name(n) => {
                    let p = st.arrays.get(&st.root(*n))?.pristine?;
                    self.consts.contains(&p).then(|| Lin::of(Term::Len(p)))
                }
                _ => None,
            },
            Rhs::Prim(Op::Bin(op), vs, _) if vs.len() == 2 => {
                let (a, b) = (self.int_of(st, &vs[0]), self.int_of(st, &vs[1]));
                match op {
                    BinOp::Add => a?.add(&b?),
                    BinOp::Sub => a?.sub(&b?),
                    BinOp::Mul => match (a?, b?) {
                        (a, b) if b.is_const() => a.scale(b.c),
                        (a, b) if a.is_const() => b.scale(a.c),
                        _ => None,
                    },
                    _ => None,
                }
            }
            Rhs::Prim(Op::Un(UnOp::Neg), vs, _) if vs.len() == 1 => {
                self.int_of(st, &vs[0])?.scale(-1)
            }
            _ => None,
        }
    }

    fn int_of(&self, st: &State, v: &Val) -> Option<Lin> {
        match v {
            Val::Lit(Lit::Int(k)) => Some(Lin::k(*k)),
            Val::Lit(Lit::Byte(b)) => Some(Lin::k(i64::from(*b))),
            Val::Lit(_) => None,
            Val::Name(n) => match st.ints.get(n) {
                Some(l) => Some(l.clone()),
                None if self.consts.contains(n) && !is_array(&self.body.names[n.index()].ty) => {
                    Some(Lin::of(Term::Val(*n)))
                }
                None => None,
            },
        }
    }
}

fn join(a: Option<State>, b: Option<State>) -> Option<State> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.join(&b)),
        (a, b) => a.or(b),
    }
}

fn value(st: &State, v: &Val) -> Elem {
    match v {
        Val::Name(n) => st.elems.get(n).cloned().unwrap_or(Elem::Fresh),
        Val::Lit(_) => Elem::Fresh,
    }
}

/// The element a function of `vs` is: a transform of the one element among
/// them, else nothing an array tracks.
fn derived(vs: impl Iterator<Item = Elem>) -> Option<Elem> {
    let mut from = vs.filter(|e| !matches!(e, Elem::Fresh));
    let first = from.next()?;
    match from.next() {
        Some(_) => Some(Elem::Fresh),
        None => Some(first.transformed()),
    }
}

/// A record literal: an element rebuilt where its parts copy one element's
/// fields of the same names, with the fields that do not changed.
fn rebuilt(st: &State, fields: &[String], parts: &[Val]) -> Elem {
    let mut of: Option<(Name, Option<Lin>)> = None;
    let mut changed = BTreeSet::new();
    for (f, v) in fields.iter().zip(parts) {
        match value(st, v) {
            Elem::Field { src, at, field } if field == *f => {
                let here = (src, at);
                match &of {
                    Some(o) if *o != here => return Elem::Fresh,
                    _ => of = Some(here),
                }
            }
            _ => {
                changed.insert(f.clone());
            }
        }
    }
    match of {
        Some((src, at)) => Elem::Of {
            src,
            at,
            values: match changed.is_empty() {
                true => Values::Same,
                false => Values::Fields(changed),
            },
        },
        None => Elem::Fresh,
    }
}

/// The loop head test `body` opens with: a run of `let`s, then `if c {} else
/// { break }`, with `c` a `<` of a counter or a `>= 0` of one.
fn head_test(body: &[St], w: &mut Walk<'_>, head: &State) -> Option<Head> {
    let mut st = head.clone();
    let mut defs: BTreeMap<Name, &Rhs> = BTreeMap::new();
    for s in body {
        match s {
            St::Let(n, rhs) => {
                defs.insert(*n, rhs);
                w.let_(&mut st, *n, rhs);
            }
            St::Check(_) => {}
            St::If {
                cond: Val::Name(c),
                then,
                els,
                ..
            } if then.is_empty() && matches!(els.as_slice(), [St::Break { .. }]) => {
                let Rhs::Prim(Op::Bin(op), vs, _) = defs.get(c)? else {
                    return None;
                };
                let (Val::Name(counter), b) = (vs.first()?, vs.get(1)?) else {
                    return None;
                };
                let bound = match op {
                    BinOp::Lt => {
                        let len = w.int_of(&st, b);
                        let p = (len.as_ref()).and_then(|l| match l.terms.as_slice() {
                            [(Term::Len(p), 1)] if l.c == 0 => Some(*p),
                            _ => None,
                        });
                        Bound::Below(p)
                    }
                    BinOp::GtEq if *b == Val::Lit(Lit::Int(0)) => Bound::AtLeastZero,
                    _ => return None,
                };
                return Some(Head {
                    counter: *counter,
                    bound,
                });
            }
            _ => return None,
        }
    }
    None
}
