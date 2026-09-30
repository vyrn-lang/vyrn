//! The linear judgment: over a [`Body`] in the named
//! core, every owned name is consumed exactly once on every path from its
//! binding. Consumed means passed to a `consume` parameter, returned, stored
//! into a place, moved into another name, or dropped. A second consume (double
//! free), a path with none (leak) and a use after one (use after free) are
//! refused with the name and the line. The kernel knows no surface language
//! and derives no release: a `Drop` the body lacks where one is owed is a
//! refusal, not a runtime leak.
//!
//! Ownership and release are separate questions. [`Kernel::owned`] answers
//! whether the body owns a name (every value);
//! [`Kernel::releases`] answers whether it owes a release (heap
//! only). A value that owes a release moves at every take; one that owes none
//! moves only into a `consume` parameter ([`Kernel::moves`]).
//!
//! The walk is a forward dataflow solver over the structured core. Its
//! [`State`] holds five relations per name: gone, static, hole, alias and
//! dead. A join is the end of an `if` or a `match`, and a back edge is the
//! end of a loop's body or a `continue`. Each refusal is a rule row (`MOVED`,
//! `JOIN_HOLE`, ...), judged where a name is used, where a scope ends, where
//! edges join ([`Kernel::agree`]) and at a back edge. The first rule that
//! holds refuses the statement; in recover mode the walk steps over it.
//!
//! - Joins: every owned name and hole is in the same state on every edge that
//!   reaches a join. A diverged edge reaches no join.
//! - Loops: a name bound outside has the same state at the back edge as at
//!   entry; a name bound inside is consumed before the back edge; every
//!   `break` agrees. The body is walked again from its entry, widened by
//!   every back edge, until no back edge widens it.
//! - Holes: `consume x.f` leaves `x` held with a hole at `.f`. An overlapping
//!   read or take is refused, a store fills it, a drop releases the rest. An
//!   element hole is `.[]`, any index.
//! - Static: a name bound to a literal releases nothing until a store gives it
//!   a value; a loop that stores is judged again from that state. A literal
//!   lives in the data segment with an all-ones `cap`, and `free` ignores any
//!   address below `heapBase()`.
//! - Borrows: a name the body does not own whose type owns heap. A read out of
//!   a place is an alias of it: a take of the alias is refused (the place owns
//!   the buffer), and a write to the place, including a `modify` argument, ends
//!   the alias. A borrow with no place carries a
//!   [`vyrn_frontend::core::BorrowKind`] instead (a parameter, a second name for one,
//!   a capture); a take of one is refused. A read of module state is an alias
//!   of the global. Every other unowned name is invisible here.

use std::collections::{BTreeMap, BTreeSet};

use vyrn_frontend::ast::{Capability, NodeId};
use vyrn_frontend::core::{
    Arg, Arm, Body, BorrowKind, Name, NameInfo, Old, Op, Payload, Place, Rhs, Site, St, Use, Val,
    Walk,
};
use vyrn_frontend::diagnostics::{menu, Diagnostic};
use vyrn_frontend::movecheck::Refusal;
use vyrn_frontend::own::Exit;

use crate::rules::{self, *};

/// A release the plan owes and did not place: `name` is still held where the
/// exit at `site` runs, or on one edge of the join at `site`, or at the end
/// of arm `arm` of the switch at `site`.
#[derive(Debug, Clone)]
pub struct Missing {
    pub exit: Exit,
    pub site: NodeId,
    pub name: Name,
    pub kind: MissingKind,
    /// The holes in `name` where the release runs, each as `.f.g` or `.[]`, so
    /// the row walks the rest. Empty for a whole name and a sub-place row.
    pub holes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MissingKind {
    /// At an exit: the plan's placed-release rows.
    Exit,
    /// On one edge of a join, by Rule N: the plan's edge table.
    Edge { edge: u32 },
    /// A sub-place released on one edge of a join because another edge took
    /// it, so both edges reach the join with the same hole: the edge table.
    EdgePlace { edge: u32, path: String },
    /// An arm's payload binder the arm never moved: the plan's arm table.
    ArmBinder { arm: u32 },
    /// A store whose place is still held: the plan's store table. Keyed by
    /// `site` alone; the place may be module state, so `name` means nothing.
    Store,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Refuse a held name at an exit.
    Judge,
    /// Record it as a missing release, treat it as released, and go on.
    Place,
}

/// Where the names of a scope end.
#[derive(Clone, Copy)]
enum End {
    /// An exit of a block, at the node the plan keys the exit's rows by.
    Exit(Exit, NodeId),
    /// The end of arm `u32` of the switch at the node, for the arm's binders.
    Arm(NodeId, u32),
}

/// Where two states must agree on every owned name ([`Kernel::agree`]).
#[derive(Clone, Copy)]
enum Point {
    /// Two live edges into one join.
    Join,
    /// A loop's back edge, the body's end or a `continue`, against the
    /// loop's entry.
    Back,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Own {
    Held,
    /// Consumed, or not yet bound: nothing to release.
    Gone,
    /// Bound to a literal: nothing to release until a store replaces it.
    Static,
}

/// The state at one point: every owned name's [`Own`], plus whether the path
/// has ended. A name absent from a map has no fact there, so a clone or a
/// join costs the facts present, not the names the body declares.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
struct State {
    /// Every name not [`Own::Gone`]: gone is absence here, and static is
    /// [`Own::Static`].
    held: BTreeMap<Name, Own>,
    /// The sub-places taken out of held names, as `(name, path)`, sorted.
    holes: Vec<(Name, String)>,
    /// The path returned, broke, continued or trapped: it reaches no join.
    ended: bool,
    /// What consumed each name, for a refusal's wording only: the line and
    /// the taker in the checker's words.
    taker: BTreeMap<Name, (usize, String, Taker)>,
    /// Where each hole was taken: `(name, path, line)`. Append-only, wording
    /// only.
    taken_at: Vec<(Name, String, usize)>,
    /// For an alias whose place was written: the line and the place. A later
    /// read is refused.
    dead: BTreeMap<Name, (usize, String)>,
    /// What each alias reads, set by the `let` that reads the place or the
    /// store that rebinds a borrow's binding.
    alias: BTreeMap<Name, Alias>,
}

impl State {
    fn own(&self, n: Name) -> Own {
        self.held.get(&n).copied().unwrap_or(Own::Gone)
    }

    fn set_own(&mut self, n: Name, o: Own) {
        match o {
            Own::Gone => self.held.remove(&n),
            _ => self.held.insert(n, o),
        };
    }

    /// The names not gone, ascending, which is creation order.
    fn live(&self) -> Vec<Name> {
        self.held.keys().copied().collect()
    }

    /// Every name held, taken or holed in `self` or `other`, ascending. Any
    /// other name is gone with no taker and no hole in both, so a join or a
    /// back edge finds nothing to say about it.
    fn named(&self, other: &State) -> BTreeSet<Name> {
        let mut out = BTreeSet::new();
        for s in [self, other] {
            out.extend(s.held.keys());
            out.extend(s.taker.keys());
            out.extend(s.holes.iter().map(|(n, _)| *n));
        }
        out
    }

    /// Widens a loop's entry by the back edge `at`: `Static` to `Held`, and
    /// an alias ended there stays ended. Returns whether anything changed.
    fn widen(&mut self, at: &State) -> bool {
        let mut changed = false;
        for (n, o) in self.held.iter_mut() {
            if *o == Own::Static && at.own(*n) == Own::Held {
                *o = Own::Held;
                changed = true;
            }
        }
        for (n, d) in &at.dead {
            if !self.dead.contains_key(n) {
                self.dead.insert(*n, d.clone());
                changed = true;
            }
        }
        changed
    }
}

/// Whether two paths under one name are equal or one is under the other.
/// The empty path is the whole name.
fn overlaps(a: &str, b: &str) -> bool {
    a.is_empty()
        || b.is_empty()
        || a == b
        || a.strip_prefix(b).is_some_and(|r| r.starts_with('.'))
        || b.strip_prefix(a).is_some_and(|r| r.starts_with('.'))
}

/// Whether skipping `r` skips `h`: equal, or `h` under `r`. One direction,
/// unlike [`overlaps`]: skipping `.line.text` still walks the rest of `.line`.
fn covers(r: &str, h: &str) -> bool {
    r == h || h.strip_prefix(r).is_some_and(|x| x.starts_with('.'))
}

/// What an alias reads out of: a name of this body, or module state.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Root {
    N(Name),
    G(String),
}

/// The place a borrow reads, resolved through every alias on its root, and
/// the name it was read through. A write through an alias ends neither it
/// nor any alias on its chain.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Alias {
    root: Root,
    path: String,
    via: Option<Name>,
}

/// The root name of a place and the path under it; `None` for module state.
pub fn root_of(p: &Place) -> Option<(Name, String)> {
    match root(p) {
        (Root::N(n), path) => Some((n, path)),
        (Root::G(_), _) => None,
    }
}

fn root(p: &Place) -> (Root, String) {
    match p {
        Place::Name(n) => (Root::N(*n), String::new()),
        Place::Global(g) => (Root::G(g.clone()), String::new()),
        Place::Field(b, f) => {
            let (r, mut path) = root(b);
            path.push('.');
            path.push_str(f);
            (r, path)
        }
        Place::Elem(b, _) | Place::Key(b, _) => {
            let (r, mut path) = root(b);
            path.push_str(".[]");
            (r, path)
        }
    }
}

/// A write point of one row, where the judgment ends the borrows that read
/// what the row writes.
enum Write<'s> {
    Store(&'s Place),
    Take(&'s Place),
    /// A value handed on, and whether a declared `consume` parameter takes
    /// it. It writes the name where the take moves it ([`Kernel::moves`]).
    Hand(&'s Val, bool),
    Release(Name),
    /// The callee may replace or free what the argument names.
    Modify(&'s Arg),
    /// The callee may store into these globals
    /// ([`crate::effects::writes_state`]).
    State(Vec<String>),
}

/// Whether a store at `rel`, a path under a container's header, lands in an
/// element. An element store moves no header (`fieldstore.rs`).
pub fn in_element(rel: &str) -> bool {
    rel.starts_with(".[]")
}

/// Every write point of the row `s` in judgment order, without the rows of a
/// list inside `s`. The state walk and [`writes`] share it so they agree.
/// `body` is the body's name in the effect judgment.
fn writes_of<'s>(s: &'s St, names: &[NameInfo], body: &str) -> Vec<Write<'s>> {
    let mut w = match s {
        // A second name for a borrow reads it; nothing is handed on.
        St::Let(n, Rhs::Val(Val::Name(m)))
            if names[n.index()].borrow && names[m.index()].borrow =>
        {
            vec![]
        }
        St::Let(_, r) | St::Do { rhs: r, .. } => match r {
            Rhs::Val(v) => vec![Write::Hand(v, false)],
            Rhs::Make(_, vs) => vs.iter().map(|v| Write::Hand(v, false)).collect(),
            Rhs::Take(p) => vec![Write::Take(p)],
            Rhs::Call {
                callee, args, kind, ..
            } => {
                let consumed = args.iter().filter(|(_, c)| *c == Capability::Consume);
                let modified = args.iter().filter(|(_, c)| *c == Capability::Modify);
                let state = crate::effects::writes_state(body, callee);
                (consumed.filter_map(|(a, _)| Some(Write::Hand(a.val()?, kind.declared()))))
                    .chain(modified.map(|(a, _)| Write::Modify(a)))
                    .chain((!state.is_empty()).then_some(Write::State(state)))
                    .collect()
            }
            Rhs::Read(_) | Rhs::Prim(..) => vec![],
        },
        St::Store { place, value, .. } => {
            let mut w = vec![Write::Hand(value, false), Write::Store(place)];
            if let Place::Key(_, k) = place {
                w.push(Write::Hand(k, false));
            }
            w
        }
        St::Drop(n, ..) | St::Row { name: n, .. } => vec![Write::Release(*n)],
        St::Return { value: Some(v), .. } => vec![Write::Hand(v, false)],
        St::Switch { on, consuming, .. } if *consuming => vec![Write::Hand(on, false)],
        _ => vec![],
    };
    let state = release_state(crate::core::runs(s, names), body);
    if !state.is_empty() {
        w.push(Write::State(state));
    }
    w
}

/// The globals the declared releases `runs` may store into, by the effect
/// judgment of the body named `body`.
fn release_state(runs: &[String], body: &str) -> Vec<String> {
    let mut state: Vec<String> = (runs.iter())
        .flat_map(|r| crate::effects::writes_state(body, r))
        .collect();
    state.sort();
    state.dedup();
    state
}

/// Whether a row of `ss` writes `on` where the judgment would end a borrow
/// of it ([`writes_of`]), or a closure captures it. The builder asks this to
/// hoist a header.
///
/// A name `ss` binds by a read of `on`, or by a switch over one, counts as
/// `on`, and so may a borrow of a place bound outside `ss`. A store into an
/// element of `on` does not count ([`in_element`]), nor does a release or
/// `return` followed only by exits. A call that stores into module state
/// counts for any name, since a name does not record the global it reads;
/// for a global, only a call that stores into it. Before `augment` holds
/// the effect judgment no call counts, and `augment` rebuilds every body
/// where that could differ.
pub fn writes(ss: &[St], on: Root, names: &[NameInfo], body: &str) -> bool {
    walk_writes(ss, on, names, body, false)
}

/// Whether a row of `ss` hands `on`, or a name that may alias it, to a
/// `modify` parameter, or a closure captures an alias: [`writes`] counting
/// only `modify` write points. The judgment ends every borrow of `on` there.
pub fn modifies(ss: &[St], on: Root, names: &[NameInfo], body: &str) -> bool {
    walk_writes(ss, on, names, body, true)
}

fn walk_writes(ss: &[St], on: Root, names: &[NameInfo], body: &str, modify: bool) -> bool {
    let mut inside = Vec::new();
    ss.iter()
        .for_each(|s| crate::core::names_bound(s, &mut inside));
    let alias = match on {
        Root::N(n) => vec![n],
        Root::G(_) => Vec::new(),
    };
    let mut w = Writes {
        on,
        body,
        alias,
        elem: Vec::new(),
        inside,
        names,
        depth: 0,
        modify,
    };
    w.list(ss)
}

struct Writes<'a> {
    on: Root,
    body: &'a str,
    /// `on` where it is a name, and every borrow bound so far by a read of
    /// `on` or of one of these.
    alias: Vec<Name>,
    /// Those of `alias` that read inside an element of `on`.
    elem: Vec<Name>,
    inside: Vec<Name>,
    names: &'a [NameInfo],
    /// How many loops inside `ss` enclose the row being asked about.
    depth: usize,
    /// Whether only a `modify` argument is a write ([`modifies`]).
    modify: bool,
}

impl Writes<'_> {
    fn aliased(&self, r: &Root) -> bool {
        match r {
            Root::N(k) => self.alias.contains(k),
            Root::G(_) => *r == self.on,
        }
    }

    /// Whether a write rooted at `r` may land under `on`.
    fn under(&self, r: &Root) -> bool {
        self.aliased(r)
            || matches!(r, Root::N(k) if {
                let info = &self.names[k.index()];
                info.borrow && info.borrow_kind.is_none() && !self.inside.contains(k)
            })
    }

    fn list(&mut self, ss: &[St]) -> bool {
        let depth = self.depth;
        ss.iter().enumerate().any(|(i, s)| {
            let tail = matches!(s, St::Drop(..) | St::Row { .. } | St::Return { .. })
                && ss[i + 1..].iter().all(|t| match t {
                    St::Drop(..) | St::Row { .. } | St::Return { .. } | St::Trap => true,
                    St::Break { .. } => depth == 0,
                    _ => false,
                });
            !tail && self.st(s)
        })
    }

    fn st(&mut self, s: &St) -> bool {
        let hit = writes_of(s, self.names, self.body)
            .into_iter()
            .any(|w| match w {
                Write::Modify(Arg::Val(v)) => matches!(v, Val::Name(k) if self.under(&Root::N(*k))),
                Write::Modify(Arg::Place(p)) => self.under(&root(p).0),
                _ if self.modify => false,
                Write::Store(p) => {
                    let (r, path) = root(p);
                    self.under(&r)
                        && !(matches!(r, Root::N(k) if self.elem.contains(&k))
                            || (self.aliased(&r) && in_element(&path)))
                }
                Write::Take(p) => self.under(&root(p).0),
                Write::Hand(v, _) => matches!((v, &self.on), (Val::Name(k), Root::N(n)) if k == n),
                Write::Release(k) => self.alias.contains(&k),
                Write::State(gs) => match &self.on {
                    Root::N(_) => true,
                    Root::G(g) => gs.contains(g),
                },
            });
        if hit {
            return true;
        }
        match s {
            St::Let(k, r) => {
                let from = match r {
                    Rhs::Read(p) => Some(root(p)),
                    Rhs::Val(Val::Name(j)) => Some((Root::N(*j), String::new())),
                    _ => None,
                };
                if let Some((r, path)) = from {
                    if self.aliased(&r) && self.names[k.index()].borrow {
                        self.alias.push(*k);
                        if matches!(r, Root::N(j) if self.elem.contains(&j)) || in_element(&path) {
                            self.elem.push(*k);
                        }
                    }
                }
                matches!(r, Rhs::Prim(Op::Closure(_), vs, _)
                    if vs.iter().any(|v| matches!(v, Val::Name(k) if self.alias.contains(k))))
            }
            St::If { then, els, .. } => {
                let t = self.list(then);
                t || self.list(els)
            }
            St::Block { body, .. } => self.list(body),
            St::Loop { body, .. } => {
                self.depth += 1;
                let w = self.list(body);
                self.depth -= 1;
                w
            }
            St::Switch { on, arms, .. } => {
                let over = matches!(on, Val::Name(k) if self.alias.contains(k));
                arms.iter().any(|a| {
                    // A binder read out of the scrutinee is its address
                    // whatever it holds ([`Kernel::read_out`]).
                    if over {
                        let names = self.names;
                        let binders = (a.binds.iter().filter(|b| names[b.index()].borrow))
                            .copied()
                            .chain(a.reads(on).iter().filter_map(|r| match r {
                                St::Let(b, _) => Some(*b),
                                _ => None,
                            }));
                        let binders: Vec<Name> = binders.collect();
                        if matches!(on, Val::Name(k) if self.elem.contains(k)) {
                            self.elem.extend(&binders);
                        }
                        self.alias.extend(binders);
                    }
                    self.list(&a.body)
                })
            }
            _ => false,
        }
    }
}

struct Kernel<'b> {
    body: &'b Body,
    mode: Mode,
    missing: Vec<Missing>,
    /// Every refusal this body earns, in walk order, so the driver can merge
    /// with the checker's by binding and line.
    refusals: Vec<Refusal>,
    /// Whether a refused statement is stepped over. Only a body known to be
    /// refused is walked so, because the step copies the state per statement.
    recover: bool,
    /// The line of the statement being judged and its taker in the checker's
    /// words, recorded against every name it consumes.
    here: usize,
    by: String,
    /// Where each part of the record literal being judged goes
    /// ([`NameInfo::fields`]), and the part being judged: its
    /// index plus one, or zero for none.
    made: Vec<String>,
    part: std::cell::Cell<usize>,
    takes: std::cell::Cell<Taker>,
    /// Whether the name consumed is leaving its scope rather than being
    /// taken. The report then records no taker; the enclosing `return` would
    /// otherwise lend it its words.
    ending: std::cell::Cell<bool>,
    /// Whether the taker is a builtin call, which disposes of a must-use
    /// value rather than moving it.
    builtin: bool,
    how: TookHow,
    /// The first take of each name on any path, for the per-binding memory
    /// report. It outlives the paths, unlike `State::taker`.
    took: std::cell::RefCell<Vec<Option<Took>>>,
    /// The names a `St::Row` already in the body reclaims, and the holes it
    /// walks around. With the [`Missing`] rows, the report can say
    /// "reclaimed at block exit".
    released: std::cell::RefCell<Vec<Option<Vec<String>>>>,
    loops: Vec<LoopCtx>,
    /// The `match` arms open around the statement, innermost last: site,
    /// index, binders. A binder still held at an exit inside the arm is the
    /// arm's row too: the emitters key that table by the arm, and no exit row
    /// can name an arm binder.
    arms: Vec<(NodeId, u32, Vec<Name>)>,
    /// The binders an arm binds as a read out of its scrutinee
    /// ([`vyrn_frontend::core::Arm::reads`]): an alias whether or not the value owns
    /// heap, because the emitter holds the payload's address.
    read_out: Vec<bool>,
    /// The names whose block has ended, which lost their taker there. A
    /// taker is read only for a row's operand ([`Kernel::stmt`]) or a name
    /// with a fact ([`State::named`]), so the check covers every read.
    ended: Vec<bool>,
}

/// What took one name, for the memory report.
#[derive(Clone, Debug)]
pub struct Took {
    pub line: usize,
    /// The taker in the checker's words. Empty for a `return` and a `drop`,
    /// which the report words itself.
    pub by: String,
    pub how: TookHow,
    /// For a must-use value a builtin call is the disposal, not a move.
    pub builtin: bool,
}

/// The constructs the report gives their own sentence; the rest are worded
/// by [`Took::by`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TookHow {
    Return,
    Drop,
    Other,
}

fn taker_of(rhs: &Rhs) -> Taker {
    match rhs {
        Rhs::Call { kind, .. } if kind.declared() => Taker::Declared,
        Rhs::Call { kind, .. } if kind.ctor() => Taker::Constructs,
        _ => Taker::Stores,
    }
}

/// How the taker of the statement being judged takes what it is handed; one
/// rule, which `movecheck` words differently for each.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Taker {
    /// A parameter the author declared `consume`: it is PASSED to it.
    Declared,
    /// A builtin's sink, a store, a literal: the value is STORED into it.
    Stores,
    /// A variant constructor: the value is PUT INTO what it makes.
    Constructs,
    /// A `modify` parameter: the value is PASSED to it, and written.
    Modifies,
    /// A store into a part: the value is WRITTEN THROUGH it.
    Writes,
}

struct LoopCtx {
    entry: State,
    breaks: Vec<State>,
    /// Every `continue`'s state, checked against the entry after the widen:
    /// a `continue` after the store that promotes a `Static` name is a back
    /// edge to the widened entry.
    continues: Vec<State>,
    bound_inside: Vec<Name>,
}

pub fn check(body: &Body) -> Result<(), Refusal> {
    run(body, Mode::Judge, false)
        .map(|_| ())
        .map_err(|mut rs| rs.remove(0))
}

/// What one placement run found: the releases owed and not placed, what took
/// each name, and which names a release already in the body reclaims.
pub struct Placement {
    pub missing: Vec<Missing>,
    pub took: Vec<Option<Took>>,
    pub released: Vec<Option<Vec<String>>>,
}

/// The releases the plan owes this body and did not place. `Err` holds every
/// refusal no placement repairs (a double free, a use after release).
pub fn placement(body: &Body) -> Result<Placement, Vec<Refusal>> {
    match run(body, Mode::Place, false) {
        Ok(m) => Ok(m),
        // Refused: walk it again, stepping over each refused statement, so
        // the body states every mistake it has and not only the first.
        Err(one) => Err(match run(body, Mode::Place, true) {
            Ok(_) => one,
            Err(all) => all,
        }),
    }
}

fn run(body: &Body, mode: Mode, recover: bool) -> Result<Placement, Vec<Refusal>> {
    let mut k = Kernel {
        body,
        mode,
        missing: Vec::new(),
        refusals: Vec::new(),
        recover,
        loops: Vec::new(),
        arms: Vec::new(),
        here: 0,
        by: String::new(),
        made: Vec::new(),
        part: std::cell::Cell::new(0),
        takes: std::cell::Cell::new(Taker::Stores),
        how: TookHow::Other,
        ending: std::cell::Cell::new(false),
        builtin: false,
        took: std::cell::RefCell::new(vec![None; body.names.len()]),
        released: std::cell::RefCell::new(vec![None; body.names.len()]),
        read_out: vec![false; body.names.len()],
        ended: vec![false; body.names.len()],
    };
    let mut st = State::default();
    for p in &body.params {
        let i = &body.names[p.index()];
        // Ownership, not release: a `consume` record of `Int64`s is owned.
        if i.releases || !i.borrow {
            st.set_own(*p, Own::Held);
        }
    }
    let walked = k.stmts(&body.stmts, &mut st);
    k.also(walked);
    if !st.ended {
        // The plan releases the parameters at the body's own block.
        let site = match body.stmts.first() {
            Some(St::Block { site, .. }) => *site,
            _ => NodeId::NONE,
        };
        let live = st.live();
        let ended = k.scope_end(&mut st, &live, End::Exit(Exit::Block, site));
        k.also(ended);
    }
    match k.refusals.is_empty() {
        true => Ok(Placement {
            missing: k.missing,
            took: k.took.into_inner(),
            released: k.released.into_inner(),
        }),
        false => Err(k.refusals),
    }
}

impl<'b> Kernel<'b> {
    /// Whether the body owns what `n` holds. Everything but
    /// a borrow is owned, including a heapless value and static data. A
    /// must-use parameter is owned whatever its capability.
    fn owned(&self, n: Name) -> bool {
        let i = &self.body.names[n.index()];
        i.releases || !i.borrow || i.must_use_param
    }

    /// Whether a held `n` owes a release at an exit: it owns heap.
    fn releases(&self, n: Name) -> bool {
        self.body.names[n.index()].releases
    }

    /// Whether a take of `n` moves it. A value that owes a release always
    /// moves; one that owes none moves only into a `consume` parameter and is
    /// copied elsewhere.
    fn moves(&self, n: Name, consume: bool) -> bool {
        consume || !self.owned(n) || self.releases(n)
    }

    /// Marks `n` consumed and keeps its taker for the wording.
    fn gone(&self, st: &mut State, n: Name) {
        st.set_own(n, Own::Gone);
        st.holes.retain(|(h, _)| *h != n);
        let by = match self
            .part
            .get()
            .checked_sub(1)
            .and_then(|i| self.made.get(i))
        {
            Some(field) => field.clone(),
            None => self.by.clone(),
        };
        // A literal's part is stored into its field, whatever the statement.
        let takes = match self.part.get() {
            0 => self.takes.get(),
            _ => Taker::Stores,
        };
        st.taker.insert(n, (self.here, by.clone(), takes));
        // The report's copy, one per binding across paths. A placed release
        // or scope end takes nothing, and a rebind clears the row, so the row
        // is the last take not followed by a rebind.
        if !by.is_empty() && !self.ending.get() {
            self.took.borrow_mut()[n.index()] = Some(Took {
                line: self.here,
                by,
                how: self.how,
                builtin: self.builtin,
            });
        }
    }

    /// Clears the report's taker of `n`, which holds a value of its own.
    fn rebound(&self, n: Name) {
        self.took.borrow_mut()[n.index()] = None;
    }

    /// Like [`Kernel::gone`], but nothing took `n`: a later mention of it is
    /// an unbound name, not a use after a take.
    fn unbind(&self, st: &mut State, n: Name) {
        st.set_own(n, Own::Gone);
        st.holes.retain(|(h, _)| *h != n);
        st.taker.remove(&n);
    }

    /// Whether `n` is gone by a take this body made. A heapless name with no
    /// taker was never bound (a `match`'s unit result, an arm's temporary).
    fn used_up(&self, st: &State, n: Name) -> bool {
        st.own(n) == Own::Gone && (self.releases(n) || st.taker.contains_key(&n))
    }

    /// The name a refusal quotes: for a temporary minted for a read of a
    /// place, the path the reader wrote ([`NameInfo::path`]);
    /// otherwise the source spelling. No program contains `@borrow`.
    fn src(&self, n: Name) -> &str {
        let i = &self.body.names[n.index()];
        i.path.as_deref().unwrap_or(&i.source)
    }

    fn info(&self, n: Name) -> String {
        let i = &self.body.names[n.index()];
        format!("`{}` (line {})", i.source, i.line)
    }

    fn borrowed(&self, n: Name) -> bool {
        self.body.names[n.index()].borrow
    }

    /// Whether `n` is a payload binder read out of a scrutinee the frame
    /// owns. Its payload is the frame's to hand on (`vyxProcessElem` in
    /// `std/vyx.vyrn`), and the name it is handed to reads nothing.
    fn gives(&self, st: &State, n: Name) -> bool {
        self.read_out[n.index()]
            && matches!(st.alias.get(&n),
                Some(Alias { via: Some(m), .. }) if self.owned(*m))
    }

    /// A payload binder handed on leaves its payload as a hole in the
    /// scrutinee, which the scrutinee's release walks around. A type that
    /// declares `release` owns the whole of its value, so the hand-off is
    /// refused.
    fn leaves_payload(&self, st: &mut State, n: Name) -> Result<(), Refusal> {
        let payload = match &self.body.names[n.index()].payload {
            None => return Ok(()),
            Some(Payload::Sealed(ty)) => {
                let b = self.src(n);
                let m = match st.alias.get(&n) {
                    Some(Alias { via: Some(m), .. }) => self.src(*m),
                    _ => b,
                };
                let args = [("b", b), ("ty", ty.as_str()), ("m", m)];
                return Err(self.say(SEALED_PAYLOAD, self.here, &args));
            }
            Some(Payload::Hole(p)) => p,
        };
        let Some(Alias {
            root: Root::N(r),
            path,
            ..
        }) = st.alias.get(&n)
        else {
            return Ok(());
        };
        let (r, hole) = (*r, format!("{path}{payload}"));
        if !st.holes.iter().any(|(h, p)| *h == r && *p == hole) {
            st.taken_at.push((r, hole.clone(), self.here));
            st.holes.push((r, hole));
            st.holes.sort();
        }
        Ok(())
    }

    /// The payload binder live in `st` that reads the hole `h` of `n`.
    fn payload_binder(&self, st: &State, n: Name, h: &str) -> Option<Name> {
        st.alias
            .iter()
            .find_map(|(b, a)| match (&self.body.names[b.index()].payload, a) {
                (
                    Some(Payload::Hole(p)),
                    Alias {
                        root: Root::N(r),
                        path,
                        ..
                    },
                ) if *r == n && format!("{path}{p}") == h => Some(*b),
                _ => None,
            })
    }

    /// A payload hole one arm left is a hole on every arm of the switch: the
    /// other arms hold another variant, whose release never reaches it.
    fn mirror_payloads(&self, entry: &State, on: &Val, arms: &[Arm], outs: &mut [State]) {
        let Val::Name(s) = on else {
            return;
        };
        let (root, prefix) = match entry.alias.get(s) {
            Some(Alias {
                root: Root::N(r),
                path,
                ..
            }) => (*r, path.clone()),
            Some(_) => return,
            None => (*s, String::new()),
        };
        let mut left: Vec<String> = Vec::new();
        for (arm, out) in arms.iter().zip(outs.iter()) {
            for b in &arm.binds {
                if let Some(Payload::Hole(p)) = &self.body.names[b.index()].payload {
                    let h = format!("{prefix}{p}");
                    if out.holes.iter().any(|(r, hp)| *r == root && *hp == h) {
                        left.push(h);
                    }
                }
            }
        }
        for out in outs.iter_mut().filter(|o| o.own(root) == Own::Held) {
            for h in &left {
                if !out.holes.iter().any(|(r, hp)| *r == root && hp == h) {
                    out.holes.push((root, h.clone()));
                }
            }
            out.holes.sort();
        }
    }

    /// The alias a binding of `p` would be, resolved through every alias on
    /// its root: after `let mt = h.meta`, `mt[0]` reads `h.meta.[]`.
    fn src_of(&self, st: &State, p: &Place) -> Alias {
        match p {
            Place::Name(n) => match st.alias.get(n) {
                Some(a) => Alias {
                    via: Some(*n),
                    ..a.clone()
                },
                None => Alias {
                    root: Root::N(*n),
                    path: String::new(),
                    via: Some(*n),
                },
            },
            Place::Global(g) => Alias {
                root: Root::G(g.clone()),
                path: String::new(),
                via: None,
            },
            Place::Field(b, f) => {
                let mut a = self.src_of(st, b);
                a.path.push('.');
                a.path.push_str(f);
                a
            }
            Place::Elem(b, _) | Place::Key(b, _) => {
                let mut a = self.src_of(st, b);
                a.path.push_str(".[]");
                a
            }
        }
    }

    /// The source of an alias, spelled for a refusal: `h.meta`, `xs[..]`.
    fn src_text(&self, st: &State, n: Name) -> String {
        match st.alias.get(&n) {
            Some(a) => self.alias_text(a),
            None => self.src(n).to_string(),
        }
    }

    fn alias_text(&self, a: &Alias) -> String {
        let root = match &a.root {
            Root::N(m) => self.src(*m).to_string(),
            Root::G(g) => g.clone(),
        };
        format!("{root}{}", a.path.replace(".[]", "[..]"))
    }

    /// A place, spelled for a refusal: `t.xs`, `xs[..]`.
    fn place_text(&self, p: &Place) -> String {
        match p {
            Place::Name(n) => self.src(*n).to_string(),
            Place::Global(g) => g.clone(),
            Place::Field(b, f) => format!("{}.{f}", self.place_text(b)),
            Place::Elem(b, _) | Place::Key(b, _) => format!("{}[..]", self.place_text(b)),
        }
    }

    /// Ends every alias that reads a place overlapping the one `w` writes
    /// ([`overlaps`]), less the chain the write goes through. A copying take
    /// writes nothing; a store into a binding writes its own slot; a store
    /// into an element ends no header a `while` walks ([`in_element`]). A
    /// `modify` argument writes what it reads and ends a walked borrow
    /// whatever it holds.
    fn end(&self, st: &mut State, w: Write) {
        // A call that may store into a global writes the whole of it, as a
        // `modify` argument naming it would: the judgment names no place
        // under a global.
        if let Write::State(gs) = w {
            for g in gs {
                let whole = Arg::Place(Place::Global(g));
                self.end(st, Write::Modify(&whole));
            }
            return;
        }
        let name;
        let p = match w {
            Write::Store(p) | Write::Take(p) => p,
            Write::Hand(Val::Name(n), consume)
                if self.owned(*n) && !st.alias.contains_key(n) && self.moves(*n, consume) =>
            {
                name = Place::Name(*n);
                &name
            }
            Write::Release(n) | Write::Modify(&Arg::Val(Val::Name(n))) => {
                name = Place::Name(n);
                &name
            }
            Write::Modify(Arg::Place(p)) => p,
            Write::Hand(..) | Write::Modify(_) | Write::State(_) => return,
        };
        let by_call = matches!(w, Write::Modify(_));
        let store = matches!(w, Write::Store(_));
        // Spelled as the reader wrote it: `t.xs[..]`, not `t.xs[][..]`.
        let (root, path, what) = match p {
            Place::Name(n) if !by_call => (Root::N(*n), String::new(), self.src(*n).to_string()),
            _ => {
                let a = self.src_of(st, p);
                let what = self.alias_text(&a);
                (a.root, a.path, what)
            }
        };
        // A write through an alias is that alias's own, and its chain's.
        let mut chain = Vec::new();
        let mut via = root_of(p).map(|(n, _)| n);
        while let Some(n) = via {
            chain.push(n);
            via = st.alias.get(&n).and_then(|x| x.via);
        }
        for (k, x) in &st.alias {
            let info = &self.body.names[k.index()];
            // An owned name holds a copy and aliases nothing, but a payload
            // binder is the payload's address ([`Kernel::read_out`]).
            let copied =
                self.owned(*k) && !self.read_out[k.index()] && !(by_call && info.walked.is_some());
            let element = store
                && info.walked == Some(Walk::While)
                && path.strip_prefix(x.path.as_str()).is_some_and(in_element);
            if !copied
                && !element
                && !chain.contains(k)
                && x.root == root
                && overlaps(&x.path, &path)
            {
                st.dead
                    .entry(*k)
                    .or_insert_with(|| (self.here, what.clone()));
            }
        }
    }

    /// Records a release the plan owes on the path `st`. The row runs where
    /// it is recorded, so the module state its declared releases store into
    /// is written there ([`writes_of`] for a row already in the body).
    fn owe(&mut self, st: &mut State, m: Missing) {
        let runs = &self.body.names[m.name.index()].runs;
        let gs = release_state(runs, &self.body.name);
        self.end(st, Write::State(gs));
        self.missing.push(m);
    }

    /// Refuses an argument that reads a global `gs` names. The callee reads
    /// its arguments until it returns, so its store into the global ends the
    /// borrow while it is still read, as it does for a `for` over the global.
    fn state_args(
        &self,
        st: &State,
        args: &[(Arg, Capability)],
        gs: &[String],
    ) -> Result<(), Refusal> {
        let mut after = st.clone();
        self.end(&mut after, Write::State(gs.to_vec()));
        let what = format!("read by {}", self.by);
        for (a, _) in args.iter().filter(|(_, c)| *c != Capability::Consume) {
            let n = match a {
                Arg::Val(Val::Name(n)) => *n,
                Arg::Place(p) => match root(p) {
                    (Root::N(n), _) => n,
                    (Root::G(g), _) if gs.contains(&g) => {
                        let s = self.place_text(p);
                        let args = [("place", g.as_str()), ("s", &s), ("what", &what)];
                        return Err(self.say(STATE_READ, self.here, &args));
                    }
                    (Root::G(_), _) => continue,
                },
                Arg::Val(_) => continue,
            };
            self.alias_read(&after, n, &what)?;
        }
        Ok(())
    }

    fn ends(&self, st: &mut State, s: &St) {
        for w in writes_of(s, &self.body.names, &self.body.name) {
            self.end(st, w);
        }
    }

    /// Refuses a read of an alias whose place was written since, at the write,
    /// in the checker's two-line form.
    fn alias_read(&self, st: &State, n: Name, what: &str) -> Result<(), Refusal> {
        let Some((l, place)) = st.dead.get(&n) else {
            return Ok(());
        };
        // The way out copies the place the alias reads, where it was bound.
        let (src, at) = (self.src_text(st, n), self.body.names[n.index()].line);
        let at = at.to_string();
        let args = [
            ("place", place.as_str()),
            ("s", self.src(n)),
            ("what", what),
            ("src", &src),
            ("at", &at),
        ];
        Err(self.say(ALIAS_READ, *l, &args))
    }

    /// Refuses a take of an alias, since the place it reads owns the buffer.
    /// Worded per exit as `movecheck.rs` words it.
    fn alias_take(&self, st: &State, n: Name, write_back: bool) -> Refusal {
        let (mut s, src, by) = (self.src(n), self.src_text(st, n), &self.by);
        // A temporary is named by the place it reads when the reader can see
        // it (`sink(if c { d.title } else { "" })`); an element or another
        // temporary is quoted in the sentence below instead.
        if s.starts_with('@') && !src.starts_with('@') && !src.contains("[..]") {
            s = &src;
        }
        // Module state read whole: the module-state sentence.
        if let Some(Alias {
            root: Root::G(g),
            path,
            ..
        }) = st.alias.get(&n)
        {
            if path.is_empty() {
                let never = "nothing may take ownership of module state \
                             (it lives for the whole module and is never dropped)";
                let msg = if by == "a `return`" {
                    format!(
                        "`{g}` may not be returned — it is module state, \
                         which nothing may take, and a return is owned"
                    )
                } else if by.ends_with("(..)`") {
                    format!(
                        "module state `{g}` may not be passed to a `consume` \
                         parameter via {by} — {never}"
                    )
                } else {
                    format!("module state `{g}` may not be consumed by {by} — {never}")
                };
                // Only a return has a way out: the caller gets a copy.
                let fixes = if by == "a `return`" {
                    vec![format!(
                        "`{g}.copy()` — the caller releases what it is handed"
                    )]
                } else {
                    Vec::new()
                };
                return self
                    .refuse_at::<()>(self.here, menu(msg, fixes))
                    .unwrap_err();
            }
            // A projection of module state is module state; the only way out
            // is the copy.
            let is_module_state = "it is module state, which nothing may take";
            let (msg, who) = if by == "a `return`" {
                (
                    format!("`{s}` may not be returned — {is_module_state}, and a return is owned"),
                    "caller",
                )
            } else {
                (format!("{} — {is_module_state}", self.may_not(s)), "callee")
            };
            return self
                .refuse_at::<()>(
                    self.here,
                    menu(
                        msg,
                        vec![format!(
                            "`{s}.copy()` — the {who} releases what it is handed"
                        )],
                    ),
                )
                .unwrap_err();
        }
        // A loop variable is worded as the loop variable, not its element.
        if let Some(of) = &self.body.names[n.index()].loop_var {
            return self.param_take(n, &BorrowKind::LoopVar { of: of.clone() });
        }
        // A borrowed root is worded by its declaration (a `read` or `modify`
        // parameter, a loop variable), where the way out is written. An owned
        // root, a `drop` and an unnamed temporary keep the place's sentence.
        if by != "a `drop`" && !write_back && !s.starts_with('@') {
            // The nearest name on the chain: `p.name` in `for p in ps` is a
            // loop variable however the parameter behind `ps` was declared.
            let mut m = st.alias.get(&n).and_then(|a| a.via);
            let root = match st.alias.get(&n) {
                Some(Alias {
                    root: Root::N(r), ..
                }) => Some(*r),
                _ => None,
            };
            while let Some(k) = m.or(root) {
                let info = &self.body.names[k.index()];
                if let Some(b) = &info.borrow_kind {
                    return self.param_take(n, b);
                }
                if let Some(of) = &info.loop_var {
                    return self.param_take(n, &BorrowKind::LoopVar { of: of.clone() });
                }
                if m.is_none() {
                    break;
                }
                m = st.alias.get(&k).and_then(|a| a.via);
            }
        }
        // Only an unnamed temporary keeps the place in the sentence; for any
        // other name the menu's `.copy()` names the place.
        let what = if s.starts_with('@') {
            format!("it is read out of `{src}`, a place that owns it")
        } else {
            "it is read out of a place that owns it".to_string()
        };
        let minted = self.body.names[n.index()].path.is_some();
        // A named binding a call takes is refused at the binding, so the
        // `.copy()` lands where the read is. A minted name has no binding.
        if write_back && by.ends_with("(..)`") && !minted {
            let (here, at) = (self.here, self.body.names[n.index()].line);
            return self
                .refuse_at::<()>(
                    at,
                    menu(
                        format!(
                            "`{s}` is read out of `{src}` here — a place that owns it\nline \
                             {here}: ... and {by} takes `{s}`, so `{s}` must be a value of its own"
                        ),
                        vec![format!(
                            "`{src}.copy()` if `{s}` should own what {by} rebuilds"
                        )],
                    ),
                )
                .unwrap_err();
        }
        // A `drop` names no place: both ways out are about the binding.
        if by == "a `drop`" {
            // In `movecheck::Borrow::what`'s words: a second name for a
            // parameter where the alias reads one (`let ops = self.ops` in a
            // `read self` method, `examples/mustuse_abandoned.vyrn`).
            let kind = match st.alias.get(&n) {
                Some(Alias {
                    root: Root::N(m), ..
                }) => self.body.names[m.index()]
                    .borrow_kind
                    .as_ref()
                    .map(|b| b.what(s)),
                _ => None,
            }
            .unwrap_or_else(|| "read out of a place that owns it".to_string());
            return self
                .refuse_at::<()>(
                    self.here,
                    menu(
                        format!("`{s}` may not be dropped — it is {kind}"),
                        vec![
                            format!(
                                "`consume` the place where `{s}` is bound, so `{s}` takes the \
                                 value rather than naming it"
                            ),
                            "delete the `drop` — the place that owns it releases it".to_string(),
                        ],
                    ),
                )
                .unwrap_err();
        }
        // An export's return: `wasi-min.js` frees every String an export
        // hands back, so the copy is the one way out.
        // [`Kernel::param_take`] says the same for a parameter.
        let msg = if by == "a `return`" && self.body.export {
            format!(
                "`{s}` may not be returned from an exported function — {what}, and the JS \
                 caller releases what it is handed"
            )
        } else if by == "a `return`" {
            // The clause [`Kernel::param_take`] and the checker use: the exit
            // is wrong, not the read.
            format!("`{s}` may not be returned — {what}, and a return is owned")
        } else {
            format!("{} — {what}", self.may_not(s))
        };
        let fixes = if by == "a `return`" && self.body.export {
            vec![rules::say(COPY_FOR_JS, &[("s", s)])]
        } else {
            self.place_fixes(st, n)
        };
        self.refuse_at::<()>(self.here, menu(msg, fixes))
            .unwrap_err()
    }

    /// The ways out of a take of a place read, in `movecheck::Borrow::fixes`'s
    /// words: first the take, which allocates nothing, where this
    /// frame owns the root; then the copy. Empty for a temporary with neither
    /// a path nor a readable place, whose refusal quotes the place.
    fn place_fixes(&self, st: &State, n: Name) -> Vec<String> {
        let own_name = self.src(n).to_string();
        let read = self.src_text(st, n);
        let path = match &self.body.names[n.index()].path {
            Some(p) => p.clone(),
            // An unnamed temporary is spelled by the place it reads.
            None if own_name.starts_with('@')
                && !read.starts_with('@')
                && !read.contains("[..]") =>
            {
                read
            }
            None if own_name.starts_with('@') => return Vec::new(),
            None => own_name,
        };
        let path = &path;
        // An element has no take (`check_take` refuses one), so for a declared
        // `consume` taker the menu names copy and `swapRemove` instead
        // (`movecheck::refuse_projected_arg`). Only an element path has `[`.
        let root = match st.alias.get(&n) {
            Some(Alias {
                root: Root::N(m), ..
            }) if self.body.names[n.index()].path.is_some() || self.src(n).starts_with('@') => {
                self.src(*m)
            }
            _ => path.as_str(),
        };
        if self.takes.get() == Taker::Declared && path.contains('[') {
            return vec![
                format!("`{path}.copy()` — the callee owns its copy"),
                rules::say(SWAP_REMOVE, &[("root", root)]),
            ];
        }
        let takeable = root != path && self.root_owns(st, n);
        let mut fixes = Vec::new();
        if takeable {
            fixes.push(format!(
                "`consume {path}` if `{root}` should give it up — the field is dead afterwards"
            ));
        }
        fixes.push(format!("`{path}.copy()` if both sides need a value"));
        fixes
    }

    /// Whether this frame owns the root of the place `n` reads, so a prefix
    /// take is an answer rather than a second refusal.
    fn root_owns(&self, st: &State, n: Name) -> bool {
        matches!(
            st.alias.get(&n),
            Some(Alias { root: Root::N(m), .. })
                if self.body.names[m.index()].borrow_kind.is_none()
        )
    }

    /// `<name> may not be <verb> <taker>` for every refusal that names a
    /// taker, with the [`Taker`]'s verb in `movecheck`'s words (#501).
    fn may_not(&self, s: &str) -> String {
        let by = &self.by;
        if by == "a literal" {
            // A record literal's part names its field; an array, map or
            // variant has none and is "the literal".
            return match self
                .part
                .get()
                .checked_sub(1)
                .and_then(|i| self.made.get(i))
            {
                Some(field) => format!("`{s}` may not be stored into {field}"),
                None => format!("`{s}` may not be stored into the literal"),
            };
        }
        if !by.ends_with("(..)`") && self.takes.get() != Taker::Writes {
            return format!("`{s}` may not be stored into {by}");
        }
        match self.takes.get() {
            Taker::Declared => {
                format!("`{s}` may not be passed to a `consume` parameter via {by}")
            }
            Taker::Constructs => format!("`{s}` may not be put into {by}"),
            Taker::Stores => format!("`{s}` may not be stored into {by}"),
            Taker::Writes => format!("`{s}` may not be written through {by}"),
            Taker::Modifies => format!("`{s}` may not be passed to a `modify` parameter via {by}"),
        }
    }

    /// Refuses the rule `r` at `line`, its holes filled from `args` and
    /// `{here}`, the line of the statement being judged ([`rules::say`]).
    fn say(&self, r: &str, line: usize, args: &[(&str, &str)]) -> Refusal {
        let here = self.here.to_string();
        let args = [args, &[("here", here.as_str())]].concat();
        self.refuse_at::<()>(line, rules::say(r, &args))
            .unwrap_err()
    }

    fn refuse_at<T>(&self, line: usize, msg: String) -> Result<T, Refusal> {
        Err(Refusal {
            diagnostic: Diagnostic::error(line, 0, "movecheck", msg)
                .in_file(self.body.file.clone()),
            body: self.body.name.clone(),
        })
    }

    /// A use at `path` after a consume, in the checker's wordings: "already
    /// consumed by" for a `consume` parameter, naming the path read; "was
    /// moved here", at the move, for any other taker, naming the storage that
    /// moved, as `movecheck::check_read` does; and "after it was released".
    fn used_after(&self, st: &State, n: Name, what: &str, path: &str) -> Refusal {
        let s = self.src(n);
        let read = format!("{s}{}", path.replace(".[]", "[..]"));
        // A written `drop` is worded as a `consume` parameter, but the note is
        // about a read, so a second `drop` omits it.
        let note = if what == "dropped" {
            ""
        } else {
            "\n  (a `consume` parameter takes ownership; the value can't be used afterward)"
        };
        let taker = st.taker.get(&n);
        let (l, by) = taker.map_or((0, ""), |(l, by, _)| (*l, by.as_str()));
        let line = l.to_string();
        let args = [
            ("s", s),
            ("read", &read),
            ("what", what),
            ("note", note),
            ("by", by),
            ("l", &line),
        ];
        match taker {
            // A declared `consume` parameter and a `drop` carry no `.copy()`
            // menu; every other taker does, a builtin sink included. A linear
            // value is worded as `consume` even under a builtin (`close(s)`)
            // ([`NameInfo::linear`]).
            Some((_, by, t))
                if *t == Taker::Declared || by == "`drop`" || self.body.names[n.index()].linear =>
            {
                self.say(CONSUMED, self.here, &args)
            }
            Some(_) if !by.is_empty() => self.say(MOVED, l, &args),
            _ => self.say(RELEASED, self.here, &args),
        }
    }

    fn hole_line(&self, st: &State, n: Name, path: &str) -> usize {
        st.taken_at
            .iter()
            .rev()
            .find(|(h, p, _)| *h == n && p == path)
            .map(|(_, _, l)| *l)
            .unwrap_or(self.body.names[n.index()].line)
    }

    /// Every name in `names` still held is a leak where the scope ends:
    /// refused when judging, recorded and released when placing. Every
    /// caller passes `names` in creation order (`bound_here`, `bound_inside`,
    /// [`State::live`], an arm's binders).
    fn scope_end(&mut self, st: &mut State, names: &[Name], end: End) -> Result<(), Refusal> {
        // A block's exit records no taker; an arm's end keeps the statement's.
        let exit = matches!(end, End::Exit(..));
        self.ending.set(exit);
        let mark = self.missing.len();
        let out = (names.iter()).try_for_each(|n| self.name_end(st, *n, end));
        // At a block's exit, newest binding first, the unwind order: inner
        // frames first and parameters last, as an owned `consume` parameter
        // requires.
        if exit {
            self.missing[mark..].reverse();
        }
        self.ending.set(false);
        out
    }

    fn name_end(&mut self, st: &mut State, n: Name, end: End) -> Result<(), Refusal> {
        if !self.owned(n) {
            return Ok(());
        }
        if st.own(n) == Own::Static {
            self.gone(st, n);
        }
        if st.own(n) != Own::Held {
            return Ok(());
        }
        // A heapless name leaves its scope with no row placed.
        if !self.releases(n) {
            self.unbind(st, n);
            return Ok(());
        }
        let row = match (self.mode, end) {
            (Mode::Judge, _) | (_, End::Arm(NodeId::NONE, _)) => None,
            (Mode::Place, End::Arm(site, arm)) => {
                Some((Exit::Block, site, MissingKind::ArmBinder { arm }))
            }
            // An exit row is keyed by the exit alone, so an exit inside an arm
            // would also fire on a sibling arm that took the name
            // (`std/html.vyrn` `keyed`). Such a name goes to an arm-keyed
            // table: the binder's row, or the Rule N edge.
            //
            // The edge table names a row by its spelling and releases it
            // whole, so a minted temporary (`std/hash.vyrn` `sha1Hex`) or a
            // name with holes (`graphql.vyrn` `gqlResolve`) takes the exit row
            // instead, as [`Kernel::equalize`] rules one level down.
            (Mode::Place, End::Exit(exit, site)) => Some(match self.arms.last() {
                Some((s, arm, binds)) if *s != NodeId::NONE && binds.contains(&n) => {
                    (Exit::Block, *s, MissingKind::ArmBinder { arm: *arm })
                }
                Some((s, arm, _))
                    if *s != NodeId::NONE
                        && !self.body.names[n.index()].source.starts_with('@')
                        && self.body.names[n.index()].holes.is_empty() =>
                {
                    (exit, *s, MissingKind::Edge { edge: *arm })
                }
                _ => (exit, site, MissingKind::Exit),
            }),
        };
        let Some((exit, site, kind)) = row else {
            let info = self.info(n);
            return Err(match end {
                End::Exit(exit, _) => {
                    let args = [("info", info.as_str()), ("exit", exit_words(exit))];
                    self.say(HELD_AT_EXIT, self.here, &args)
                }
                End::Arm(..) => self.say(HELD_AT_ARM_END, self.here, &[("info", &info)]),
            });
        };
        // The row carries this path's holes, which may differ from the
        // binding's set on another path.
        let holes = self.holes_owned(st, n);
        let m = Missing {
            exit,
            site,
            name: n,
            kind,
            holes,
        };
        self.owe(st, m);
        self.gone(st, n);
        Ok(())
    }

    /// A release of `n` that walks around `holes`, which must equal the
    /// state's holes: reaching a taken place is a double free, skipping a held
    /// one a leak, which a placed row (`at`) repairs with the state's set.
    fn drop(
        &mut self,
        st: &mut State,
        n: Name,
        holes: &[String],
        at: Option<(Exit, NodeId)>,
    ) -> Result<(), Refusal> {
        if !self.owned(n) {
            // A release is a take, so releasing a borrow is refused.
            // A `for x in consume xs` loop's take lands on the
            // container's release, so it is worded as the loop, not a `drop`
            // ([`NameInfo::for_consume`]).
            if st.alias.contains_key(&n) {
                let form = if self.body.names[n.index()].for_consume {
                    "the `for .. in consume` loop"
                } else {
                    "a `drop`"
                };
                let by = std::mem::replace(&mut self.by, form.to_string());
                let r = self.alias_take(st, n, false);
                self.by = by;
                return Err(r);
            }
            let info = self.info(n);
            return Err(self.say(RELEASED_UNOWNED, self.here, &[("info", &info)]));
        }
        // A heapless release frees nothing; the plan places such a row where
        // its edge table wants one, and the ownership state still ends.
        if !self.releases(n) {
            self.unbind(st, n);
            return Ok(());
        }
        if st.own(n) == Own::Gone {
            // A written `drop` is worded as one; a placed release as a release.
            let what = if self.by == "`drop`" {
                "dropped"
            } else {
                "released"
            };
            return Err(self.used_after(st, n, what, ""));
        }
        if st.own(n) == Own::Held {
            let state = self.holes_owned(st, n);
            // Every place that left must be under a hole the row skips.
            if let Some(h) = state.iter().find(|h| !holes.iter().any(|r| covers(r, h))) {
                // A written `drop` releases by type and cannot skip a hole, so
                // its menu names the write-back and the deletion. A placed
                // release is worded as a release.
                let (l, info) = (self.hole_line(st, n, h).to_string(), self.info(n));
                let args = [("s", self.src(n)), ("h", h), ("l", &l), ("info", &info)];
                let f = match self.by == "`drop`" {
                    true => DROP_WITH_HOLE,
                    false => RELEASED_WITH_HOLE,
                };
                return Err(self.say(f, self.here, &args));
            }
            // Every hole the row skips must be under a place that left.
            if let Some(r) = holes.iter().find(|r| !state.iter().any(|h| covers(h, r))) {
                match (self.mode, at) {
                    (Mode::Place, Some((exit, site))) => self.missing.push(Missing {
                        exit,
                        site,
                        name: n,
                        kind: MissingKind::Exit,
                        holes: state,
                    }),
                    _ => {
                        let info = self.info(n);
                        let args = [("info", info.as_str()), ("h", r)];
                        return Err(self.say(RELEASED_AROUND, self.here, &args));
                    }
                }
            }
        }
        self.gone(st, n);
        Ok(())
    }

    /// A read of a name: it must be held, and an alias's place unwritten.
    fn read(&self, st: &State, v: &Val) -> Result<(), Refusal> {
        self.read_at(st, v, "")
    }

    /// [`Kernel::read`], with the path read for the wording.
    fn read_at(&self, st: &State, v: &Val, path: &str) -> Result<(), Refusal> {
        if let Val::Name(n) = v {
            if self.owned(*n) && self.used_up(st, *n) {
                return Err(self.used_after(st, *n, "used", path));
            }
            self.alias_read(st, *n, "used")?;
        }
        Ok(())
    }

    /// Refuses a borrow captured by a closure that outlives the call it is
    /// written at, since the closure leaves the owner's frame. The
    /// borrow is worded by its kind, or else by its alias's root.
    fn escaping_capture(&self, st: &State, caps: &[Name], line: usize) -> Result<(), Refusal> {
        for c in caps {
            if !self.borrowed(*c) {
                continue;
            }
            let s = self.src(*c);
            let (what, fixes) = match &self.body.names[c.index()].borrow_kind {
                Some(b) => (b.what(s), b.fixes(s)),
                None => (
                    match st.alias.get(c) {
                        Some(Alias {
                            root: Root::N(m), ..
                        }) => self.body.names[m.index()]
                            .borrow_kind
                            .as_ref()
                            .map(|b| b.what(s)),
                        _ => None,
                    }
                    .unwrap_or_else(|| "read out of a place that owns it".to_string()),
                    Vec::new(),
                ),
            };
            let msg = rules::say(ESCAPING_CAPTURE, &[("s", s), ("what", &what)]);
            return self.refuse_at(line, menu(msg, fixes));
        }
        Ok(())
    }

    /// Refuses a take of a `read` or `modify` parameter, a second name for
    /// one, or a capture: another frame owns it.
    /// None has a place, so the alias table does not see them.
    fn param_take(&self, n: Name, b: &BorrowKind) -> Refusal {
        let (s, capture) = (self.src(n), matches!(b, BorrowKind::Capture));
        let (ret, export) = (self.by == "a `return`", self.body.export);
        let rule = match () {
            _ if ret && capture => RETURNED_CAPTURE,
            _ if ret && export => RETURNED_TO_JS,
            _ if ret => RETURNED_BORROW,
            _ => TAKEN_BORROW,
        };
        // The ways out, as `movecheck::Borrow::fixes` and `fixes_here` name
        // them. The value a constructor makes owns what it is given: only the
        // copy. An `export extern fn` signature refuses `consume`, so only a
        // copy is left.
        let (fix, more) = match () {
            _ if self.takes.get() == Taker::Constructs && !capture => (Some(COPY_TO_OWN), vec![]),
            _ if ret && capture => (Some(COPY_FOR_CALLER), vec![]),
            _ if capture => (None, vec![]),
            _ if export && ret => (Some(COPY_FOR_JS), vec![]),
            _ if export => (Some(COPY_FROM_JS), vec![]),
            _ => (None, b.fixes(s)),
        };
        let rule = fix.map_or(rule.to_string(), |f| format!("{rule}|{f}"));
        let (what, may_not) = (b.what(s), self.may_not(s));
        let args = [
            ("s", s),
            ("what", what.as_str()),
            ("may_not", may_not.as_str()),
        ];
        let msg = menu(rules::say(&rule, &args), more);
        self.refuse_at::<()>(self.here, msg).unwrap_err()
    }

    /// The kind of borrow `n` is, where a take of it is refused by that kind
    /// ([`Kernel::param_take`]). `None` for a must-use parameter, which the
    /// callee may hand on.
    fn kind_of_borrow(&self, n: Name) -> Option<&BorrowKind> {
        let i = &self.body.names[n.index()];
        i.borrow_kind
            .as_ref()
            .filter(|_| self.borrowed(n) && !i.must_use_param)
    }

    /// Refuses, as a take, a write into a part of a second name for a
    /// parameter, or a `modify` argument of one: it would replace or release
    /// the caller's heap (#501). A name bound to a place this frame owns
    /// writes that buffer, which is defined
    /// (`an-alias-of-a-field-written-through`).
    fn write(&self, n: Name) -> Result<(), Refusal> {
        match self.kind_of_borrow(n) {
            Some(b @ BorrowKind::Param { .. }) if !self.body.params.contains(&n) => {
                Err(self.param_take(n, b))
            }
            _ => Ok(()),
        }
    }

    /// A take of a name: it must be held, and is gone afterwards. An alias is
    /// never taken.
    fn take(&self, st: &mut State, v: &Val) -> Result<(), Refusal> {
        self.take_arg(st, v, false, false)
    }

    /// [`Kernel::take`]. The receiver of a rebuilding builtin (`out.push(v)`,
    /// `write_back`) changes no owner, since the store after the call puts it
    /// back, so it may be a `modify` parameter
    /// ([`vyrn_frontend::core::Rhs::Call::write_back`], `prelude::rebuilds`).
    fn take_arg(
        &self,
        st: &mut State,
        v: &Val,
        write_back: bool,
        consume: bool,
    ) -> Result<(), Refusal> {
        if let Val::Name(n) = v {
            if !write_back {
                if let Some(b) = self.kind_of_borrow(*n) {
                    return Err(self.param_take(*n, b));
                }
            }
            if st.alias.contains_key(n) {
                self.alias_read(st, *n, "used")?;
                if self.moves(*n, consume) && !self.gives(st, *n) {
                    return Err(self.alias_take(st, *n, write_back));
                }
                // A payload binder handed on moves that part, whatever takes
                // it (#572).
                if consume || self.body.names[n.index()].heap {
                    self.leaves_payload(st, *n)?;
                }
                return Ok(());
            }
            if self.owned(*n) {
                if self.used_up(st, *n) {
                    return Err(self.used_after(st, *n, "used", ""));
                }
                if let Some((_, path)) = st.holes.iter().find(|(h, _)| h == n) {
                    let l = self.hole_line(st, *n, path);
                    let line = l.to_string();
                    let args = [("s", self.src(*n)), ("path", path), ("l", &line)];
                    return Err(self.say(WHOLE_WITH_HOLE, l, &args));
                }
                if self.moves(*n, consume) {
                    self.gone(st, *n);
                }
            }
        }
        Ok(())
    }

    /// The indices a place reads, and the root: held, and not under a hole.
    fn place(&self, st: &State, p: &Place) -> Result<(), Refusal> {
        self.indices(st, p)?;
        let Some((n, path)) = root_of(p) else {
            return Ok(());
        };
        self.read_at(st, &Val::Name(n), &path)?;
        if self.owned(n) {
            if let Some((_, h)) = st
                .holes
                .iter()
                .find(|(h, hp)| *h == n && overlaps(hp, &path))
            {
                // Both lines name the storage that moved, not the longer path
                // read, as in `used_after` and `movecheck::check_use`.
                let args = [("s", self.src(n)), ("h", h)];
                return Err(self.say(READ_IN_HOLE, self.hole_line(st, n, h), &args));
            }
        }
        Ok(())
    }

    fn indices(&self, st: &State, p: &Place) -> Result<(), Refusal> {
        match p {
            Place::Name(_) | Place::Global(_) => Ok(()),
            Place::Field(b, _) => self.indices(st, b),
            Place::Elem(b, i) | Place::Key(b, i) => {
                self.indices(st, b)?;
                self.read(st, i)
            }
        }
    }

    /// A take out of a sub-place: the root keeps a hole there.
    fn take_place(&self, st: &mut State, p: &Place) -> Result<(), Refusal> {
        self.place(st, p)?;
        if let Some((n, path)) = root_of(p) {
            if self.owned(n) && !path.is_empty() {
                st.taken_at.push((n, path.clone(), self.here));
                st.holes.push((n, path));
                st.holes.sort();
            }
        }
        Ok(())
    }

    /// Records a store whose place this path still holds. The row is keyed by
    /// the store's node alone, so a synthesized store (a global's
    /// initializer, a desugar's temporary, a `place at` rewrite) records
    /// nothing.
    ///
    /// `holes` are the holes inside the stored place, spelled relative to it
    /// (`.f`): the release of the displaced value walks around them. A place
    /// that is itself a hole holds nothing, and a hole under an element
    /// cannot be walked around, so neither records a row: the first is empty
    /// and the second leaks rather than frees twice.
    fn owe_store(&mut self, site: &Site, holes: Vec<String>) {
        if self.mode != Mode::Place || holes.iter().any(|h| h.is_empty() || h.contains("[]")) {
            return;
        }
        let Site::Node(at) = site else {
            return;
        };
        if std::env::var("VYRN_KERNEL_TRACE").is_ok() {
            eprintln!(
                "owe-store: {} line {} site {}",
                self.body.name, self.here, at.0
            );
        }
        self.missing.push(Missing {
            exit: Exit::Block,
            site: *at,
            name: Name(0),
            kind: MissingKind::Store,
            holes,
        });
    }

    /// The holes inside the place `path` of `n`, relative to it: `""` when
    /// the place is itself a hole, `.g` for a hole at `path.g`.
    fn holes_in(st: &State, n: Name, path: &str) -> Vec<String> {
        st.holes
            .iter()
            .filter(|(h, _)| *h == n)
            .filter_map(|(_, hp)| hp.strip_prefix(path))
            .filter(|rest| rest.is_empty() || rest.starts_with('.'))
            .map(str::to_string)
            .collect()
    }

    /// A store into a sub-place fills the hole there, and anything under it.
    /// A store under a hole writes into what left.
    fn store_place(&self, st: &mut State, p: &Place) -> Result<(), Refusal> {
        self.indices(st, p)?;
        let Some((n, path)) = root_of(p) else {
            return Ok(());
        };
        self.read(st, &Val::Name(n))?;
        if !self.owned(n) {
            return Ok(());
        }
        if let Some((_, h)) = st.holes.iter().find(|(h, hp)| {
            *h == n
                && path
                    .strip_prefix(hp.as_str())
                    .is_some_and(|r| r.starts_with('.'))
        }) {
            let args = [("s", self.src(n)), ("h", h), ("path", &path)];
            return Err(self.say(STORE_UNDER_HOLE, self.hole_line(st, n, h), &args));
        }
        st.holes.retain(|(h, hp)| !(*h == n && overlaps(hp, &path)));
        Ok(())
    }

    fn rhs(&self, st: &mut State, r: &Rhs) -> Result<(), Refusal> {
        match r {
            Rhs::Val(v) => self.take(st, v),
            Rhs::Read(p) => self.place(st, p),
            Rhs::Take(p) => self.take_place(st, p),
            Rhs::Call {
                callee,
                args,
                write_back,
                kind,
                ..
            } => {
                // Reads first, takes after, so one call may read and take the
                // same receiver (`dup.append(dup)`).
                if let Some(f) = kind.value() {
                    self.read(st, &Val::Name(f))?;
                }
                for (a, cap) in args {
                    match a {
                        Arg::Place(p) => self.place(st, p)?,
                        Arg::Val(v) if !matches!(cap, Capability::Consume) => self.read(st, v)?,
                        Arg::Val(_) => {}
                    }
                    if *cap == Capability::Modify {
                        let root = match a {
                            Arg::Place(p) => root_of(p).map(|(n, _)| n),
                            Arg::Val(Val::Name(n)) => Some(*n),
                            Arg::Val(_) => None,
                        };
                        if let Some(n) = root {
                            let takes = self.takes.replace(Taker::Modifies);
                            let r = self.write(n);
                            self.takes.set(takes);
                            r?;
                        }
                    }
                }
                let gs = crate::effects::writes_state(&self.body.name, callee);
                if !gs.is_empty() {
                    self.state_args(st, args, &gs)?;
                }
                for (i, (a, cap)) in args.iter().enumerate() {
                    if let (Arg::Val(v), Capability::Consume) = (a, cap) {
                        // Only a declared `consume` parameter moves a heapless
                        // value; a builtin sink or constructor copies it.
                        self.take_arg(st, v, *write_back && i == 0, kind.declared())?;
                    }
                }
                Ok(())
            }
            Rhs::Prim(_, vs, _) => {
                for v in vs {
                    self.read(st, v)?;
                }
                Ok(())
            }
            Rhs::Make(_, vs) => {
                // `part` lets a refusal name the field each part goes into.
                for (i, v) in vs.iter().enumerate() {
                    self.part.set(i + 1);
                    let r = self.take(st, v);
                    self.part.set(0);
                    r?;
                }
                Ok(())
            }
        }
    }

    /// A statement list that is not a source block: what it binds ends with
    /// it, at site 0.
    fn stmts(&mut self, stmts: &[St], st: &mut State) -> Result<(), Refusal> {
        self.stmts_at(stmts, st, NodeId::NONE)
    }

    fn also(&mut self, r: Result<(), Refusal>) {
        if let Err(r) = r {
            self.refusals.push(r);
        }
    }

    fn stmts_at(&mut self, stmts: &[St], st: &mut State, site: NodeId) -> Result<(), Refusal> {
        let mut bound_here: Vec<Name> = Vec::new();
        for s in stmts {
            if st.ended {
                // Code after a return, break or continue never runs.
                break;
            }
            if !self.recover {
                self.stmt(s, st, &mut bound_here)?;
                continue;
            }
            // A refused statement is undone: its half-judged state (a
            // receiver never handed back, a temporary never bound) would
            // earn refusals about machinery, not the program.
            let before = st.clone();
            let bound = bound_here.len();
            let missing = self.missing.len();
            let one = self.stmt(s, st, &mut bound_here);
            if one.is_err() {
                *st = before;
                bound_here.truncate(bound);
                self.missing.truncate(missing);
                // The binding stands, or later statements would be refused
                // for using a name never bound.
                if let St::Let(n, _) = s {
                    if self.owned(*n) {
                        st.set_own(*n, Own::Held);
                        bound_here.push(*n);
                    }
                }
            }
            self.also(one);
        }
        if !st.ended {
            self.scope_end(st, &bound_here, End::Exit(Exit::Block, site))?;
            // `scope_end` left each binding gone; `stmt` panics on a row that
            // names one before a `let` binds it again.
            for n in &bound_here {
                st.taker.remove(n);
                self.ended[n.index()] = true;
            }
        }
        Ok(())
    }

    /// What a right-hand side takes its operands with, in the checker's words.
    fn by_of(&self, rhs: &Rhs, bound: Option<Name>) -> String {
        match rhs {
            Rhs::Val(_) => match bound {
                Some(n) if !self.src(n).starts_with('@') => {
                    format!("the binding `{}`", self.src(n))
                }
                // The reader wrote the loop, not its container temporary.
                Some(n) if self.body.names[n.index()].for_consume => {
                    "the `for .. in consume` loop".to_string()
                }
                _ => "a value".to_string(),
            },
            // An impl method is named by its protocol member, so an instance
            // and the generic body word one refusal alike.
            Rhs::Call { callee, .. } => format!(
                "`{}(..)`",
                vyrn_frontend::types::impl_method_member(callee)
                    .unwrap_or(callee)
                    .trim_start_matches('@')
            ),
            Rhs::Take(_) => "`consume`".to_string(),
            Rhs::Make(..) => "a literal".to_string(),
            Rhs::Read(_) | Rhs::Prim(..) => String::new(),
        }
    }

    fn stmt(&mut self, s: &St, st: &mut State, bound_here: &mut Vec<Name>) -> Result<(), Refusal> {
        let (ended, body) = (&mut self.ended, self.body);
        s.operands(&mut |v, how| match v {
            Val::Name(n) if how == Use::Bind => ended[n.index()] = false,
            Val::Name(n) if ended[n.index()] => panic!(
                "kernel: a row of `{}` names `{}` (line {}) after its block ended",
                body.name,
                body.names[n.index()].source,
                body.names[n.index()].line
            ),
            _ => {}
        });
        // The line and taker every consumption in this statement records.
        self.how = match s {
            St::Return { .. } => TookHow::Return,
            St::Drop(_, _, line, _) if *line > 0 => TookHow::Drop,
            _ => TookHow::Other,
        };
        self.builtin = match s {
            St::Let(_, Rhs::Call { kind, .. })
            | St::Do {
                rhs: Rhs::Call { kind, .. },
                ..
            } => kind.stores(),
            _ => false,
        };
        match s {
            St::Let(n, rhs) => {
                self.here = self.body.names[n.index()].line;
                self.by = self.by_of(rhs, Some(*n));
                self.takes.set(taker_of(rhs));
                self.made = self.body.names[n.index()].fields.clone();
                self.rebound(*n);
                self.released.borrow_mut()[n.index()] = None;
            }
            St::Store { place, line, .. } => {
                if let Place::Name(n) = place {
                    self.rebound(*n);
                    self.released.borrow_mut()[n.index()] = None;
                }
                self.here = *line;
                self.takes.set(Taker::Stores);
                self.by = match place {
                    Place::Name(n) if !self.src(*n).starts_with('@') => {
                        format!("the binding `{}`", self.src(*n))
                    }
                    Place::Field(_, f) => format!("the field `{f}`"),
                    // An element or key store names its container, as the
                    // checker does.
                    Place::Global(g) => format!("module state `{g}`"),
                    p => match root_of(p) {
                        Some((n, _)) if !self.src(n).starts_with('@') => {
                            format!("`{}`", self.src(n))
                        }
                        _ => "a store".to_string(),
                    },
                };
            }
            St::Return { line, .. } => {
                self.here = *line;
                self.by = "a `return`".to_string();
                self.takes.set(Taker::Stores);
            }
            St::Do { rhs, line, .. } => {
                self.here = *line;
                self.takes.set(taker_of(rhs));
                self.by = self.by_of(rhs, None);
            }
            St::Switch { line, .. } => {
                self.here = *line;
                self.by = "a `match`".to_string();
            }
            // A written `drop` has a line; a placed release has line 0, stands
            // at the binding, and records no taker.
            St::Drop(n, _, line, _) if *line > 0 => {
                self.here = *line;
                self.by = "`drop`".to_string();
            }
            // A placed row: "reclaimed at block exit", with its holes.
            St::Row { name: n, holes, .. } => {
                self.here = self.body.names[n.index()].line;
                self.by = String::new();
                self.released.borrow_mut()[n.index()] = Some(holes.clone());
            }
            St::Drop(n, ..) => {
                self.here = self.body.names[n.index()].line;
                self.by = String::new();
            }
            _ => {}
        }
        self.judge(s, st, bound_here)?;
        // A switch and a `return` end their take's writes before arms and exit.
        if !matches!(s, St::Switch { .. } | St::Return { .. }) {
            self.ends(st, s);
        }
        Ok(())
    }

    fn judge(&mut self, s: &St, st: &mut State, bound_here: &mut Vec<Name>) -> Result<(), Refusal> {
        match s {
            St::Let(n, rhs) => {
                // A literal names the data segment until a store gives it a
                // buffer. `Static` is about a release, so a heapless name is
                // never `Static`. A `Make` of literals is not static: `[4, 5]`
                // and a record with a thunk (`examples/lazyfield.vyrn`)
                // allocate, and treating them as static leaks them.
                let is_static = self.releases(*n) && matches!(rhs, Rhs::Val(Val::Lit(_)));
                // A closure that outlives its call may not hold a borrow.
                // The core marks which closures escape
                // ([`NameInfo::closure_escapes`]).
                if matches!(rhs, Rhs::Prim(Op::Closure(_), ..)) {
                    let i = &self.body.names[n.index()];
                    if let Some(reads) = i.closure_reads.clone() {
                        self.escaping_capture(st, &reads, i.line)?;
                    }
                }
                // An alias: a borrow read out of a place, or a second name for
                // a borrow, which is not a take of it.
                st.dead.remove(n);
                st.alias.remove(n);
                match rhs {
                    Rhs::Read(p) if self.borrowed(*n) || self.read_out[n.index()] => {
                        st.alias.insert(*n, self.src_of(st, p));
                    }
                    // A read of module state is an alias whatever it holds:
                    // the rule is about a global's lifetime, not heap.
                    Rhs::Read(p @ Place::Global(_)) if self.owned(*n) => {
                        st.alias.insert(*n, self.src_of(st, p));
                    }
                    Rhs::Val(Val::Name(m))
                        if self.borrowed(*n) && self.borrowed(*m) && !self.gives(st, *m) =>
                    {
                        self.read(st, &Val::Name(*m))?;
                        st.alias.insert(*n, self.src_of(st, &Place::Name(*m)));
                        return Ok(());
                    }
                    _ => {}
                }
                self.rhs(st, rhs)?;
                if self.owned(*n) {
                    st.set_own(*n, if is_static { Own::Static } else { Own::Held });
                    st.holes.retain(|(h, _)| h != n);
                    bound_here.push(*n);
                    if let Some(l) = self.loops.last_mut() {
                        l.bound_inside.push(*n);
                    }
                }
            }
            St::Store {
                place,
                value,
                old,
                site,
                ..
            } => {
                // A borrow's binding rebound to another borrow (`t = d.title`
                // after `let t = s.name`): the alias travels, as at a `let`.
                if let (Place::Name(n), Val::Name(m)) = (place, value) {
                    if self.borrowed(*n) && st.alias.contains_key(m) && !self.gives(st, *m) {
                        self.read(st, value)?;
                        let a = st.alias[m].clone();
                        st.alias.insert(*n, a);
                        st.dead.remove(n);
                        return Ok(());
                    }
                }
                // The write-back of the place desugar puts the alias
                // back into the place it reads: no owner changes, and the
                // alias ends.
                if let Val::Name(m) = value {
                    let into = self.src_of(st, place);
                    let back = st
                        .alias
                        .get(m)
                        .is_some_and(|a| a.root == into.root && a.path == into.path);
                    if back {
                        self.read(st, value)?;
                        st.dead.insert(*m, (self.here, self.place_text(place)));
                        return Ok(());
                    }
                }
                if let Some((n, path)) = root_of(place) {
                    if !path.is_empty() {
                        let takes = self.takes.replace(Taker::Writes);
                        let r = self.write(n);
                        self.takes.set(takes);
                        r?;
                    }
                }
                // Read before the take ends the temporary: a store gives its
                // target the value's state, `Static` included, as a `let` does.
                let fresh_static = match value {
                    Val::Lit(_) => true,
                    Val::Name(m) => self.releases(*m) && st.own(*m) == Own::Static,
                };
                // A payload binder stored into a binding that releases
                // nothing stays the scrutinee's: nothing takes it.
                match (place, value) {
                    (Place::Name(n), Val::Name(m)) if !self.releases(*n) && self.gives(st, *m) => {
                        self.read(st, value)?
                    }
                    _ => self.take(st, value)?,
                }
                if let Place::Name(n) = place {
                    // A borrow's binding given a fresh value is no alias.
                    if self.borrowed(*n) {
                        st.alias.remove(n);
                        st.dead.remove(n);
                    }
                }
                match place {
                    // A store over a heapless name rebinds it.
                    Place::Name(n) if self.owned(*n) && !self.releases(*n) => {
                        st.set_own(*n, Own::Held);
                    }
                    Place::Name(n) if self.releases(*n) => {
                        // A store over a name not `Gone` owes the release of
                        // its value. `Static` counts: the emitters free a
                        // literal like any other value.
                        let holes = Self::holes_in(st, *n, "");
                        if *old == Old::Pending
                            && self.mode == Mode::Place
                            && st.own(*n) != Own::Gone
                        {
                            self.owe_store(site, holes);
                        } else if st.own(*n) == Own::Held
                            && *old != Old::Released
                            && *old != Old::Transferred
                        {
                            // `Pending` is decided here; any other `Old` over
                            // a held place is a leak.
                            if *old == Old::Pending && self.mode == Mode::Place {
                                self.owe_store(site, holes);
                            } else {
                                let info = self.info(*n);
                                let args = [("info", info.as_str())];
                                return Err(self.say(OVERWRITTEN, self.here, &args));
                            }
                        }
                        if st.own(*n) == Own::Gone && *old == Old::Released {
                            let info = self.info(*n);
                            let args = [("info", info.as_str())];
                            return Err(self.say(RELEASED_BEFORE_STORE, self.here, &args));
                        }
                        st.set_own(*n, if fresh_static { Own::Static } else { Own::Held });
                        // The new value is whole.
                        st.holes.retain(|(h, _)| h != n);
                    }
                    Place::Name(_) => {}
                    other => {
                        // Read before the store fills them: the displaced
                        // value's release walks around the holes (#468).
                        let holes = root_of(other)
                            .map(|(n, path)| Self::holes_in(st, n, &path))
                            .unwrap_or_default();
                        self.store_place(st, other)?;
                        // The map keeps the key it is handed.
                        if let Place::Key(_, k) = other {
                            self.take(st, k)?;
                        }
                        if *old == Old::Unreleased {
                            let l = self.line_of(value).to_string();
                            let args = [("l", l.as_str())];
                            return Err(self.say(STORE_RELEASES_NOTHING, self.here, &args));
                        }
                        // The kernel tracks whole names, so the rule is over
                        // the root: module state and a `modify` parameter
                        // always owe the release, any other root while held.
                        if *old == Old::Pending {
                            // The alias table's root, not the place's: the
                            // place desugar stores through a temporary.
                            let owes = match self.src_of(st, other).root {
                                Root::G(_) => true,
                                Root::N(n) => {
                                    matches!(
                                        self.body.names[n.index()].borrow_kind,
                                        Some(BorrowKind::Param { cap: "modify", .. })
                                    ) || (self.owned(n) && st.own(n) != Own::Gone)
                                }
                            };
                            if owes {
                                self.owe_store(site, holes);
                            }
                        }
                    }
                }
            }
            // A release of a payload binder the frame may hand on is the
            // hand-off ([`Kernel::equalize`] places one on an edge).
            St::Drop(n, ..) if self.gives(st, *n) => {
                self.alias_read(st, *n, "used")?;
                self.leaves_payload(st, *n)?;
            }
            St::Drop(n, _, _, row) => {
                let holes = self.body.drop_holes(*n, row).to_vec();
                self.drop(st, *n, &holes, None)?;
            }
            St::Row {
                name,
                holes,
                exit,
                site,
            } => {
                self.drop(st, *name, holes, Some((*exit, *site)))?;
            }
            St::If {
                cond,
                then,
                els,
                site,
            } => {
                self.read(st, cond)?;
                let mut a = st.clone();
                self.stmts(then, &mut a)?;
                let mut b = st.clone();
                self.stmts(els, &mut b)?;
                let mut edges = vec![a, b];
                self.equalize(&mut edges, *site);
                *st = self.join(&edges)?;
            }
            St::Switch {
                on,
                arms,
                consuming,
                carries,
                ..
            } => {
                if *consuming {
                    self.take(st, on)?;
                    self.ends(st, s);
                } else {
                    self.read(st, on)?;
                }
                let mut outs = Vec::new();
                for arm in arms {
                    for r in arm.reads(on) {
                        if let St::Let(b, _) = r {
                            self.read_out[b.index()] = true;
                        }
                    }
                    let Arm {
                        binds,
                        body,
                        site,
                        index,
                        ..
                    } = arm;
                    let mut a = st.clone();
                    for b in binds {
                        if self.owned(*b) {
                            a.set_own(*b, Own::Held);
                        }
                    }
                    // The binders' scope is the arm: its `scope_end` checks them.
                    if *carries {
                        self.arms.push((*site, *index, binds.clone()));
                    }
                    let walked = self.stmts(body, &mut a);
                    if *carries {
                        self.arms.pop();
                    }
                    walked?;
                    if !a.ended {
                        self.scope_end(&mut a, binds, End::Arm(*site, *index))?;
                    }
                    outs.push(a);
                }
                self.mirror_payloads(st, on, arms, &mut outs);
                let site = arms.first().map(|a| a.site).unwrap_or(NodeId::NONE);
                self.equalize(&mut outs, site);
                *st = self.join(&outs)?;
            }
            St::Block { site, body, .. } => {
                self.stmts_at(body, st, *site)?;
            }
            St::Loop { body, .. } => {
                // The body is walked from the loop's entry, then again from
                // the entry widened by every back edge (the body's end and
                // each `continue`), until a walk widens nothing. Each walk's
                // refusals replace the last walk's, or each would be said
                // twice. A widen that changes the entry turns a `Static` name
                // `Held` or ends an alias, so there are at most twice as many
                // walks as the body has names, plus one.
                let (mut entry, mark) = (st.clone(), self.refusals.len());
                let (mut a, ctx) = loop {
                    self.refusals.truncate(mark);
                    let mut a = entry.clone();
                    self.loops.push(LoopCtx {
                        entry,
                        breaks: Vec::new(),
                        continues: Vec::new(),
                        bound_inside: Vec::new(),
                    });
                    self.stmts(body, &mut a)?;
                    let mut ctx = self.loops.pop().expect("the walk's own loop");
                    let back = (!a.ended).then_some(&a);
                    let mut wider = false;
                    for at in back.into_iter().chain(&ctx.continues) {
                        wider |= ctx.entry.widen(at);
                    }
                    if !wider {
                        break (a, ctx);
                    }
                    entry = ctx.entry;
                };
                // The body's end and every `continue` must find the entry state.
                for c in &ctx.continues {
                    self.back(c, &ctx)?;
                }
                if !a.ended {
                    let end = End::Exit(Exit::Block, NodeId::NONE);
                    self.scope_end(&mut a, &ctx.bound_inside, end)?;
                    self.back(&a, &ctx)?;
                }
                if ctx.breaks.is_empty() {
                    st.ended = true;
                } else {
                    *st = self.join(&ctx.breaks)?;
                }
            }
            // Outside a loop the path ends; `typed::loops` refuses it.
            St::Break { .. } | St::Continue { .. } if self.loops.is_empty() => st.ended = true,
            St::Break { site, .. } => {
                let inside = self.loops.last().expect("a loop").bound_inside.clone();
                self.scope_end(st, &inside, End::Exit(Exit::Break, *site))?;
                self.loops
                    .last_mut()
                    .expect("a loop")
                    .breaks
                    .push(st.clone());
                st.ended = true;
            }
            St::Continue { site, .. } => {
                let inside = self.loops.last().expect("a loop").bound_inside.clone();
                self.scope_end(st, &inside, End::Exit(Exit::Continue, *site))?;
                // Judged by the loop, after it widens its entry.
                self.loops
                    .last_mut()
                    .expect("a loop")
                    .continues
                    .push(st.clone());
                st.ended = true;
            }
            St::Return {
                value,
                site,
                is_try,
                ..
            } => {
                if let Some(v) = value {
                    self.take(st, v)?;
                    self.ends(st, s);
                }
                let exit = if *is_try { Exit::Try } else { Exit::Return };
                let live = st.live();
                self.scope_end(st, &live, End::Exit(exit, *site))?;
                st.ended = true;
            }
            St::Do { rhs, .. } => self.rhs(st, rhs)?,
            St::Trap => st.ended = true,
            St::Check(_) => {}
        }
        Ok(())
    }

    /// Rule N in placement mode: where one live edge of a join took
    /// a name another holds, the holding edges release it into the plan's
    /// edge table. In judging mode `join` refuses the disagreement.
    fn equalize(&mut self, edges: &mut [State], site: NodeId) {
        if self.mode != Mode::Place || site == NodeId::NONE {
            return;
        }
        let live: Vec<usize> = (0..edges.len()).filter(|i| !edges[*i].ended).collect();
        let names: BTreeSet<Name> = (live.iter())
            .flat_map(|i| edges[*i].held.keys().copied())
            .collect();
        for n in names {
            // A heapless name needs no edge row; `join` reconciles it.
            if !self.releases(n) {
                continue;
            }
            let held: Vec<usize> = live
                .iter()
                .copied()
                .filter(|i| edges[*i].own(n) != Own::Gone)
                .collect();
            // Rule N one level down: an edge lacking another's hole releases
            // that sub-place. An edge whose own hole overlaps the path is
            // left to the judgment.
            let mut union: Vec<String> = held
                .iter()
                .flat_map(|i| self.holes_owned(&edges[*i], n))
                .collect();
            union.sort();
            union.dedup();
            for i in &held {
                for h in &union {
                    let mine = self.holes_owned(&edges[*i], n);
                    if mine.iter().any(|hp| overlaps(hp, h)) {
                        continue;
                    }
                    // A row cannot spell a payload, so it releases the binder.
                    let (name, kind) = match self.payload_binder(&edges[*i], n, h) {
                        Some(b) => (b, MissingKind::Edge { edge: *i as u32 }),
                        None => (
                            n,
                            MissingKind::EdgePlace {
                                edge: *i as u32,
                                path: h.clone(),
                            },
                        ),
                    };
                    self.owe(
                        &mut edges[*i],
                        Missing {
                            exit: Exit::Block,
                            site,
                            name,
                            kind,
                            holes: Vec::new(),
                        },
                    );
                    edges[*i].holes.push((n, h.clone()));
                    edges[*i].holes.sort();
                }
            }
            let gone = live.iter().any(|i| edges[*i].own(n) == Own::Gone);
            if !gone || held.is_empty() {
                continue;
            }
            for i in held {
                let holes = self.holes_owned(&edges[i], n);
                self.owe(
                    &mut edges[i],
                    Missing {
                        exit: Exit::Block,
                        site,
                        name: n,
                        kind: MissingKind::Edge { edge: i as u32 },
                        holes,
                    },
                );
                self.gone(&mut edges[i], n);
            }
        }
    }

    fn holes_owned(&self, st: &State, n: Name) -> Vec<String> {
        self.holes_of(st, n)
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    fn line_of(&self, v: &Val) -> usize {
        match v {
            Val::Name(n) => self.body.names[n.index()].line,
            Val::Lit(_) => 0,
        }
    }

    /// Refuses a name bound outside the loop `ctx` that the back edge `at`
    /// does not have as the loop's entry has it.
    fn back(&self, at: &State, ctx: &LoopCtx) -> Result<(), Refusal> {
        let outside = at.named(&ctx.entry).into_iter();
        (outside.filter(|n| !ctx.bound_inside.contains(n)))
            .try_for_each(|n| self.agree(Point::Back, at, &ctx.entry, n))
    }

    /// Refuses the first way the name `n` differs between `a` and `b` that
    /// `point` rules out. At a join, `a` and `b` are two live edges, and a
    /// heapless name may differ: a later use of it is refused instead. At a
    /// back edge, `a` is the back edge and `b` the widened entry, and a back
    /// edge `Static` where the entry is `Held` owes less and agrees.
    fn agree(&self, point: Point, a: &State, b: &State, n: Name) -> Result<(), Refusal> {
        if !self.owned(n) {
            return Ok(());
        }
        let (oa, ob, heap) = (a.own(n), b.own(n), self.releases(n));
        // The state that took `n` where the other still has it.
        let took = match point {
            Point::Join if heap && (oa == Own::Gone) != (ob == Own::Gone) => {
                Some(if oa == Own::Gone { a } else { b })
            }
            Point::Back if oa == Own::Gone && ob != Own::Gone => Some(a),
            _ => None,
        };
        let s = self.src(n);
        if let Some(g) = took {
            return Err(match (g.taker.get(&n), point) {
                (Some((l, by, _)), _) if !by.is_empty() => {
                    let f = match point {
                        Point::Join => JOIN_MOVED,
                        Point::Back => LOOP_MOVED,
                    };
                    self.say(f, *l, &[("s", s), ("by", by)])
                }
                (_, Point::Join) => self.say(JOIN_RELEASED, self.here, &[("s", s)]),
                (_, Point::Back) if heap => self.say(LOOP_RELEASED, self.here, &[("s", s)]),
                _ => return Ok(()),
            });
        }
        let info = || self.info(n);
        match point {
            _ if !heap => return Ok(()),
            Point::Join if oa == Own::Gone => return Ok(()),
            Point::Back if oa != ob && !(oa == Own::Static && ob == Own::Held) => {
                return Err(self.say(LOOP_BOUND, self.here, &[("info", &info())]));
            }
            _ => {}
        }
        let (ha, hb) = (self.holes_of(a, n), self.holes_of(b, n));
        if ha == hb {
            return Ok(());
        }
        // A hole a turn made would be taken again next turn. Only a prefix
        // `consume` makes a hole, so the taker is always `consume`.
        let made = ha.iter().find(|h| !hb.contains(h));
        Err(match (point, made.map(|h| h.replace(".[]", "[..]"))) {
            (Point::Join, _) => self.say(JOIN_HOLE, self.here, &[("info", &info())]),
            (Point::Back, Some(h)) => {
                let args = [("s", s), ("h", h.as_str())];
                self.say(LOOP_HOLE, self.hole_line(a, n, &h), &args)
            }
            (Point::Back, None) => self.say(LOOP_HOLE_AT, self.here, &[("info", &info())]),
        })
    }

    fn holes_of<'s>(&self, st: &'s State, n: Name) -> Vec<&'s str> {
        st.holes
            .iter()
            .filter(|(h, _)| *h == n)
            .map(|(_, p)| p.as_str())
            .collect()
    }

    /// The state after a join, where every live edge agrees on every name
    /// ([`Kernel::agree`]). An alias ended or bound on any edge is so after
    /// the join. A heap name is `Held` unless every edge has it `Static`. A
    /// heapless name is gone if an edge took it, held if an edge holds it,
    /// and has every edge's holes.
    fn join(&self, edges: &[State]) -> Result<State, Refusal> {
        let live: Vec<&State> = edges.iter().filter(|s| !s.ended).collect();
        let Some(first) = live.first() else {
            return Ok(State {
                ended: true,
                ..edges[0].clone()
            });
        };
        let mut joined = (*first).clone();
        for other in &live[1..] {
            for (n, d) in &other.dead {
                joined.dead.entry(*n).or_insert_with(|| d.clone());
            }
            for (n, a) in &other.alias {
                joined.alias.entry(*n).or_insert_with(|| a.clone());
            }
            for n in first.named(other) {
                self.agree(Point::Join, first, other, n)?;
                if !self.owned(n) {
                    continue;
                }
                if self.releases(n) {
                    if first.own(n) != other.own(n) {
                        joined.set_own(n, Own::Held);
                    }
                    continue;
                }
                let taken = live
                    .iter()
                    .find(|s| s.own(n) == Own::Gone && s.taker.contains_key(&n));
                match taken {
                    Some(s) => {
                        joined.set_own(n, Own::Gone);
                        joined.taker.insert(n, s.taker[&n].clone());
                    }
                    // An edge that never bound it does not count.
                    None if live.iter().any(|s| s.own(n) != Own::Gone) => {
                        joined.set_own(n, Own::Held);
                    }
                    None => {}
                }
                for h in self.holes_owned(other, n) {
                    if !joined.holes.iter().any(|(m, p)| *m == n && *p == h) {
                        joined.holes.push((n, h));
                    }
                }
                joined.holes.sort();
            }
        }
        Ok(joined)
    }
}
