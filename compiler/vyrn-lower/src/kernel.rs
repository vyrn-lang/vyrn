//! The linear judgment — RFC-0125 §2.2, judgment 1.
//!
//! Over a [`Body`] in the named core: **every owned name is consumed exactly
//! once on every path from its binding.** Consumed means passed to a `consume`
//! parameter, returned, stored into a place, moved into another name, or
//! dropped. A name consumed twice is a double free; a name never consumed on
//! some path is a leak; a name used after it was consumed is a use after free.
//! All three are refused, with the name and the line.
//!
//! The kernel knows nothing about the surface language. It does not know what
//! a `match` is, only a switch with arms; not what `?` is, only an early
//! return. It derives no release: a `Drop` is in the body or it is not, and if
//! it is not where one is owed, the judgment says so. That is the whole
//! mechanism by which a placement the plan missed becomes a compile-time
//! refusal instead of a runtime leak the ratchet measures.
//!
//! **Two questions, and the kernel asks them apart.** Whether the body OWNS
//! what a name holds is RFC-0089 rule 1, and that rule reaches every value:
//! a `consume` parameter takes ownership of a record of `Int64`s exactly as
//! it takes ownership of a `String`. Whether a held name owes a RELEASE is
//! RFC-0114, and that question is about a heap buffer: a value that owns no
//! heap owes nothing at any exit. [`Kernel::owned`] answers the first,
//! [`Kernel::releases`] the second, and the judgment tracks the ownership
//! state ([`Own`]) of every owned name while it places a release for none
//! but the ones that owe one.
//!
//! What follows from the split is what a take IS. A value that owes a release
//! moves at every take, because the buffer has one owner: a rebinding, a
//! literal part, a store, a `return`, a `consume` argument. A value that owes
//! none moves only where a `consume` parameter takes it, because a copy of it
//! costs nothing and owns nothing — `let b = a` over an `Int64` is not a
//! move, and `take(consume a)` is ([`Kernel::moves`]).
//!
//! **Joins.** After an `if` or a `switch`, every owned name must be in the same
//! state on every edge that reaches the join — released on one edge and held
//! on another is refused, which is RFC-0114's Rule N stated once. An edge that
//! diverged (returned, broke, continued, trapped) does not reach the join.
//!
//! **Loops.** A name bound outside a loop must be in the same state at the
//! loop's back edge as at its entry, or the second iteration would use or free
//! what the first consumed. A name bound inside the loop body must be consumed
//! before the back edge, because the body's end is its scope's end. A `break`
//! leaves the loop with the state it had; every `break` must agree.
//!
//! **Holes.** A `take` of a sub-place (`consume x.f`) moves the part out and
//! leaves the name held with a hole at that path. A later read or take that
//! overlaps the hole is refused; a store at the hole fills it; a drop of the
//! name releases the rest, which is what the plan's release walk does with
//! its hole set. Two edges of a join must agree on the holes as on the names.
//! An element hole is tracked as `[]`, any index: coarser than the source,
//! and the plan cannot skip inside an element either.
//!
//! **Static.** An owned name bound to a literal (`let mut s = ""`) holds
//! static data: a store over it releases nothing, a drop of it frees nothing
//! (the runtime reads a capacity of 0 as "never free"), and a scope's end
//! owes it nothing. The state is `Static` until a store gives the name a
//! value, and a loop whose body does so is judged again from that state, so
//! the second turn's store is judged against what the first turn left.
//!
//! **Borrows.** A name the body does not own whose type owns heap is a
//! borrow (RFC-0089 rule 2): a parameter, or a binding read out of a place
//! somebody owns (`let mt = h.meta`, `let x = xs[i]`, `let q = p`). A
//! binding read out of a place is an alias of that place, and the kernel
//! keeps what it reads. An alias may be read and passed on; a take of it —
//! a `consume` argument, a literal part, a store, a `return` — is refused,
//! because the place still owns the buffer and the engines would release it
//! twice (`rfcs/probes-0125/take-out-of-a-read-parameter.vyrn`). And a write
//! to the place it reads — a store, a take, a drop — ends the alias: a later
//! read of it is refused, because the compiled routes see the write through
//! one buffer and the interpreter does not
//! (`rfcs/probes-0125/alias-then-write-through-the-root.vyrn`). RFC-0090:
//! all mutation is exclusive. Not modelled: what a `modify` argument does to
//! the aliases of what it is handed (`examples/tree.vyrn`'s `freeNode` reads
//! a handle out of the node it then removes, and a handle is safe to hold);
//! `rhs` says what the census measured when the rule was tried.
//!
//! A borrow with no place to be an alias of carries its kind instead
//! ([`crate::core::BorrowKind`], RFC-0125 §3 M3, the census): a `read` or
//! `modify` parameter, a second name for one, and a lambda frame's capture.
//! A take of one is refused in the checker's words — the caller owns a
//! parameter (RFC-0089 rule 2), and the frame that made a capture owns it
//! (RFC-0037). Module state is neither: a read of a global is an alias of
//! it, and RFC-0013's own sentence says why nothing may take it.
//!
//! Every other name the body does not own — a pattern binder of a
//! non-consuming switch over a value that owns heap — is invisible here.

use crate::core::{Arm, Body, BorrowKind, Name, Old, Place, Rhs, St, Val};
use vyrn_frontend::ast::Capability;
use vyrn_frontend::own::Exit;

/// A release the plan owes and did not place: `name` is still held where the
/// exit at `site` runs, or on one edge of the join at `site`, or at the end
/// of arm `arm` of the switch at `site`.
#[derive(Debug, Clone)]
pub struct Missing {
    pub exit: Exit,
    pub site: usize,
    pub name: Name,
    pub kind: MissingKind,
    /// The holes in `name` where the release runs — the sub-places a take
    /// left, each as `.f.g` or `.[]` — so the row walks the rest and no more.
    /// Empty for a whole name, and for a sub-place row.
    pub holes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MissingKind {
    /// At an exit: the plan's placed-release rows.
    Exit,
    /// On one edge of a join, RFC-0114 Rule N: the plan's edge table.
    Edge { edge: u32 },
    /// A sub-place of `name` released on one edge of a join because another
    /// edge took it (`if d.ok { keep(consume d.line) } else { .. }`): Rule N
    /// one level down, so both edges reach the join with the same hole. The
    /// plan's edge table, with the path spelled onto the name.
    EdgePlace { edge: u32, path: String },
    /// An arm's payload binder the arm never moved: the plan's arm table.
    ArmBinder { arm: u32 },
    /// A store whose place is still HELD: the plan's store table. The row is
    /// keyed by the STORE and by nothing else — the place written into may be
    /// module state, which is no name of this frame — so `name` carries no
    /// meaning here and a reader must take `site` alone.
    Store,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Refuse a held name at an exit.
    Judge,
    /// Record it as a missing release, treat it as released, and go on.
    Place,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Own {
    /// Bound and holding its value.
    Held,
    /// Consumed, or not yet bound: nothing to release.
    Gone,
    /// Bound to a literal: nothing to release until a store replaces it.
    Static,
}

/// The state at one point: every owned name's [`Own`], plus whether the path
/// has ended.
#[derive(Clone, PartialEq, Eq, Debug)]
struct State {
    own: Vec<Own>,
    /// The sub-places taken out of held names, as `(name, path)`, sorted.
    holes: Vec<(Name, String)>,
    /// The path returned, broke, continued or trapped: it reaches no join.
    ended: bool,
    /// What consumed each name, for the wording of a refusal (RFC-0125 M3,
    /// third slice): the line and the taker in the checker's words — "the
    /// binding `t`", "`take(..)`", "a `return`". Not part of the judgment.
    taker: Vec<Option<(usize, String, Taker)>>,
    /// Where each hole was taken: `(name, path, line)`. Append-only, and not
    /// part of the judgment either.
    taken_at: Vec<(Name, String, usize)>,
    /// For an alias: the line and the place written, once the place it
    /// reads has been written. A later read is refused.
    dead: Vec<Option<(usize, String)>>,
    /// What each alias reads out of. Bound by the `let` that reads the
    /// place, or the store that rebinds a borrow's binding; on a path.
    alias: Vec<Option<Alias>>,
}

/// One refusal with its menu of ways out (RFC-0087 U2), in the shape
/// `movecheck::menu` prints: the sentence, then one `fix:` line per way out.
///
/// A refusal is a head, a sentence and a menu, and a reader who loses a menu
/// loses part of the refusal. So the kernel names the same ways out in the
/// same words as the checker, which is what lets a rule leave the checker
/// without the diagnostic moving (RFC-0125 §3 M3, the menu slice).
fn menu(mut message: String, fixes: Vec<String>) -> String {
    for f in fixes {
        message.push_str(&format!("\n  fix: {f}"));
    }
    message
}

/// Whether two paths under one name overlap: equal, or one under the other.
/// The empty path is the whole name.
fn overlaps(a: &str, b: &str) -> bool {
    a.is_empty()
        || b.is_empty()
        || a == b
        || a.strip_prefix(b).is_some_and(|r| r.starts_with('.'))
        || b.strip_prefix(a).is_some_and(|r| r.starts_with('.'))
}

/// Whether skipping `r` skips `h`: equal, or `h` under `r`. One direction,
/// unlike [`overlaps`]: a row that skips `.line.text` still walks the rest of
/// `line`, which is wrong when all of `.line` has left.
fn covers(r: &str, h: &str) -> bool {
    r == h || h.strip_prefix(r).is_some_and(|x| x.starts_with('.'))
}

/// What an alias reads out of: a name of this body, or module state.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Root {
    N(Name),
    G(String),
}

/// An alias: the place a borrow reads, resolved through every alias on its
/// root, and the name it was read through (`t.xs[]` reads `t.xs` through
/// `t`; `t.xs[][]` reads it through `t.xs[]`). A write through an alias is
/// not a write the alias, or any alias on its chain, has to end for.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Alias {
    root: Root,
    path: String,
    via: Option<Name>,
}

/// The root name of a place and the path under it; `None` for module state.
pub(crate) fn root_of(p: &Place) -> Option<(Name, String)> {
    match p {
        Place::Name(n) => Some((*n, String::new())),
        Place::Global(_) => None,
        Place::Field(b, f) => {
            let (n, mut path) = root_of(b)?;
            path.push('.');
            path.push_str(f);
            Some((n, path))
        }
        Place::Elem(b, _) | Place::Key(b, _) => {
            let (n, mut path) = root_of(b)?;
            path.push_str(".[]");
            Some((n, path))
        }
    }
}

/// One refusal, worded for the author of the program in the checker's voice
/// (`movecheck.rs`): the name, the line it was moved on and what took it, the
/// line it is used again on. `line` is the line the diagnostic is at, and
/// `file` the module it is in (`None` for the root), so the CLI prints it as
/// it prints the checker's.
#[derive(Debug, Clone)]
pub struct Refusal {
    pub message: String,
    pub line: usize,
    pub file: Option<String>,
    /// The body the refusal is in, for the corpus test's tally.
    pub body: String,
}

struct Kernel<'b> {
    body: &'b Body,
    mode: Mode,
    missing: Vec<Missing>,
    /// Every refusal this body earns, in walk order — RFC-0125 §3 M3, the
    /// second-sentence slice. The judgment used to stop at the first, so the
    /// driver could not tell a second mistake from the first one said again:
    /// it dropped a kernel refusal about any binding the checker had already
    /// named, anywhere in the file. A body that states all of them lets the
    /// driver merge by the binding AND the line, which is what a reader
    /// compares.
    refusals: Vec<Refusal>,
    /// Whether a refused statement is stepped over ([`Kernel::refusals`]).
    ///
    /// Only a body already known to be refused is walked this way, because
    /// the step costs a copy of the state per statement and the answer is the
    /// same for every body that is not. So `run` walks once to find out and
    /// again to say everything.
    recover: bool,
    /// The line of the statement being judged, and what it takes with, in
    /// the checker's words — recorded against every name it consumes.
    here: usize,
    by: String,
    /// Where each part of the record literal being judged goes, if it is one
    /// ([`crate::core::NameInfo::fields`]), and which part is being judged —
    /// its index plus one, or zero for "not one of them".
    made: Vec<String>,
    part: std::cell::Cell<usize>,
    /// How that taker takes ([`Taker`]).
    takes: Taker,
    /// Whether the name being consumed is leaving its SCOPE rather than
    /// being taken: the release this pass places, and a literal whose scope
    /// ends. Nothing took it, so the report records no taker — the statement
    /// around the exit is a `return` and would otherwise lend it its words.
    ending: std::cell::Cell<bool>,
    /// Whether the taker of the statement being judged is a BUILTIN call —
    /// which is how a must-use value is disposed of rather than moved
    /// (`movecheck::sinks` answers false at a linear parameter, so the
    /// checker records no move there either).
    builtin: bool,
    /// How that taker READS, for the memory report's wording ([`TookHow`]).
    how: TookHow,
    /// What took each name, for the memory report — the FIRST take on any
    /// path, which is the one answer a per-binding report gives (RFC-0125 §3
    /// M3, the report slice). `State::taker` says the same thing per path and
    /// is part of no judgment either; this outlives the paths, because the
    /// report is about the binding and not about the walk that reached it.
    took: std::cell::RefCell<Vec<Option<Took>>>,
    /// The names a release ALREADY IN THE BODY reclaims, and the holes it
    /// walks around — a `St::Row`, which is a row the placement walk placed.
    /// The judgment's own [`Missing`] rows are the releases owed and NOT
    /// placed, so the report needs both halves to say "reclaimed at block
    /// exit" (RFC-0125 §3 M3, the report slice).
    released: std::cell::RefCell<Vec<Option<Vec<String>>>>,
    /// The loop being walked: the state at its entry, the states at its
    /// `break`s, and the names bound inside it (which its back edge must find
    /// consumed).
    loops: Vec<LoopCtx>,
    /// The `match` arms open around the statement being judged, innermost
    /// last: the arm's site, its index, and the binders it bound.
    ///
    /// A binder still held where the arm ENDS is the arm's own row
    /// ([`Kernel::binders_end`]). Since a `return` inside the arm carries the
    /// exit (RFC-0125 §3 M3, row 17), a binder is as often still held at an
    /// exit INSIDE the arm — and it is the same row: the table the emitters
    /// read is keyed by the arm, and an arm binder is no binding of the frame
    /// that an exit row could name.
    arms: Vec<(usize, u32, Vec<Name>)>,
}

/// What took one name, for the memory report (RFC-0125 §3 M3): where, in
/// whose words, and by which construct.
#[derive(Clone, Debug)]
pub struct Took {
    pub line: usize,
    /// The taker in the checker's words — `` `f(..)` ``, "the field `s`",
    /// "the `for .. in consume` loop". Empty for a `return` and a `drop`,
    /// which the report words itself.
    pub by: String,
    pub how: TookHow,
    /// A builtin call took it: for a must-use value that is the DISPOSAL, not
    /// a move (RFC-0075 M1, RFC-0095 M1).
    pub builtin: bool,
}

/// Which construct took a name — the three the report gives its own sentence
/// to. Everything else is worded by [`Took::by`] alone.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TookHow {
    Return,
    Drop,
    Other,
}

/// Which taker a right-hand side is.
fn taker_of(rhs: &Rhs) -> Taker {
    match rhs {
        Rhs::Call { kind, .. } if kind.declared() => Taker::Declared,
        Rhs::Call { kind, .. } if kind.ctor() => Taker::Constructs,
        _ => Taker::Stores,
    }
}

/// How the taker of the statement being judged takes what it is handed, which
/// is three sentences for one rule — `movecheck` words each differently
/// (RFC-0125 §3 M3, rows 07, 19 and 34).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Taker {
    /// A parameter the author declared `consume`: it is PASSED to it.
    Declared,
    /// A builtin's sink, a store, a literal: the value is STORED into it.
    Stores,
    /// A variant constructor: the value is PUT INTO what it makes.
    Constructs,
}

struct LoopCtx {
    entry: State,
    breaks: Vec<State>,
    /// Every `continue`'s state, checked against the entry AFTER the widen —
    /// a `continue` that follows the store which promotes a `Static` name is
    /// as much a back edge as the body's end, and it must be judged against
    /// the entry the second turn really has (RFC-0125 §3 M3, the default
    /// slice).
    continues: Vec<State>,
    bound_inside: Vec<Name>,
}

pub fn check(body: &Body) -> Result<(), Refusal> {
    run(body, Mode::Judge, false)
        .map(|_| ())
        .map_err(|mut rs| rs.remove(0))
}

/// The releases the plan owes this body and did not place. `Err` when the
/// body is refused for another reason — a double free, a use after release —
/// which no placement repairs. Every such refusal of the body is there, not
/// only the first: see [`Kernel::refusals`].
/// What one placement run found: the releases owed and not placed, what took
/// each name, and which names a release already in the body reclaims.
pub struct Placement {
    pub missing: Vec<Missing>,
    pub took: Vec<Option<Took>>,
    pub released: Vec<Option<Vec<String>>>,
}

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
        takes: Taker::Stores,
        how: TookHow::Other,
        ending: std::cell::Cell::new(false),
        builtin: false,
        took: std::cell::RefCell::new(vec![None; body.names.len()]),
        released: std::cell::RefCell::new(vec![None; body.names.len()]),
    };
    let mut st = State {
        own: vec![Own::Gone; body.names.len()],
        holes: Vec::new(),
        ended: false,
        taker: vec![None; body.names.len()],
        taken_at: Vec::new(),
        dead: vec![None; body.names.len()],
        alias: vec![None; body.names.len()],
    };
    for p in &body.params {
        let i = &body.names[*p as usize];
        // The ownership question, not the release one: a `consume` parameter
        // of a record of `Int64`s is this body's (RFC-0089 rule 1).
        if i.releases || !i.borrow {
            st.own[*p as usize] = Own::Held;
        }
    }
    let walked = k.stmts(&body.stmts, &mut st);
    k.also(walked);
    if !st.ended {
        // The parameters: the plan releases them at the body's own block.
        let site = match body.stmts.first() {
            Some(St::Block { site, .. }) => *site,
            _ => 0,
        };
        let ended = k.scope_end(&mut st, &all_names(body), Exit::Block, site);
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

fn all_names(body: &Body) -> Vec<Name> {
    (0..body.names.len() as Name).collect()
}

impl<'b> Kernel<'b> {
    /// Whether the body OWNS what `n` holds — RFC-0089 rule 1, which is a
    /// rule about every value. A borrow is not owned; everything else the
    /// body binds is, whatever its type owns: a value that owns no heap, and
    /// a name holding static data (`let s = "a"`), which owes no release and
    /// is still this frame's to hand over once.
    fn owned(&self, n: Name) -> bool {
        let i = &self.body.names[n as usize];
        i.releases || !i.borrow
    }

    /// Whether a held `n` owes a RELEASE at an exit — RFC-0114, which is a
    /// rule about a heap buffer. A value that owns none owes none.
    fn releases(&self, n: Name) -> bool {
        self.body.names[n as usize].releases
    }

    /// Whether a take of `n` at this site moves it. A value that owes a
    /// release moves at every take, because the buffer has one owner. A
    /// value that owes none moves only where a `consume` parameter takes it:
    /// the capability IS the take (RFC-0089 rule 1), and a rebinding, a
    /// literal or a store of such a value copies it.
    fn moves(&self, n: Name, consume: bool) -> bool {
        consume || !self.owned(n) || self.releases(n)
    }

    /// The name is consumed: nothing to release, and no holes to remember.
    /// What consumed it, and where, is kept for the wording.
    fn gone(&self, st: &mut State, n: Name) {
        st.own[n as usize] = Own::Gone;
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
        // A part of a literal is STORED into the field, whatever the statement
        // around it does.
        let takes = match self.part.get() {
            0 => self.takes,
            _ => Taker::Stores,
        };
        st.taker[n as usize] = Some((self.here, by.clone(), takes));
        // The report's copy, flattened over the paths: a binding gets ONE
        // sentence. A statement with no taker in it — a release this pass
        // placed, a scope end — takes nothing, and a rebind clears the row
        // again, so what is left is the last take the binding was not given a
        // value back after.
        if !by.is_empty() && !self.ending.get() {
            self.took.borrow_mut()[n as usize] = Some(Took {
                line: self.here,
                by,
                how: self.how,
                builtin: self.builtin,
            });
        }
    }

    /// The name is bound, or bound again: whatever took it before, it holds a
    /// value of its own now, and the report says so.
    fn rebound(&self, n: Name) {
        self.took.borrow_mut()[n as usize] = None;
    }

    /// The name is out of scope: like [`Kernel::gone`], but nothing took it.
    /// A name that owes no release leaves this way, and a later mention of it
    /// is a name the body never bound rather than a use after a take.
    fn unbind(&self, st: &mut State, n: Name) {
        st.own[n as usize] = Own::Gone;
        st.holes.retain(|(h, _)| *h != n);
        st.taker[n as usize] = None;
    }

    /// Whether `n` being gone here is a use after a take THIS body made. A
    /// name that owes no release and carries no taker was never bound at all
    /// — the unit result of a `match`, a temporary an arm stores into — and
    /// the judgment has nothing to say about it.
    fn used_up(&self, st: &State, n: Name) -> bool {
        st.own[n as usize] == Own::Gone && (self.releases(n) || st.taker[n as usize].is_some())
    }

    /// The name a refusal quotes: the path the reader wrote where this name is
    /// a temporary the lowering minted for a read of a place
    /// ([`crate::core::NameInfo::path`]), and the source spelling otherwise.
    /// `@borrow` is a name no program contains, and a reader who is told about
    /// it is told about the compiler rather than about the program.
    fn src(&self, n: Name) -> &str {
        let i = &self.body.names[n as usize];
        i.path.as_deref().unwrap_or(&i.source)
    }

    fn info(&self, n: Name) -> String {
        let i = &self.body.names[n as usize];
        format!("`{}` (line {})", i.source, i.line)
    }

    /// Whether `n` is a borrow (RFC-0089 rule 2): the core says.
    fn borrowed(&self, n: Name) -> bool {
        self.body.names[n as usize].borrow
    }

    /// What a place reads out of, through every alias on its root: the
    /// alias a binding of it would be. `let mt = h.meta` then `mt[0]` reads
    /// `h.meta.[]`.
    fn src_of(&self, st: &State, p: &Place) -> Alias {
        match p {
            Place::Name(n) => match &st.alias[*n as usize] {
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
        match &st.alias[n as usize] {
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

    /// `p` is written: every alias reading a place that overlaps it ends
    /// here. `what` is the place, in the checker's words.
    fn wrote(&self, st: &mut State, p: &Place, what: &str) {
        // A store into a binding writes the binding's own slot, not the
        // place it reads.
        let a = match p {
            Place::Name(n) => Alias {
                root: Root::N(*n),
                path: String::new(),
                via: None,
            },
            _ => self.src_of(st, p),
        };
        // Spelled through the aliases, as the reader wrote it: `t.xs[..]`,
        // not the desugar's `t.xs[][..]`.
        let what = if matches!(p, Place::Name(_)) {
            what.to_string()
        } else {
            self.alias_text(&a)
        };
        // A write through an alias is that alias's own, and its chain's.
        let mut chain = Vec::new();
        let mut via = root_of(p).map(|(n, _)| n);
        while let Some(n) = via {
            chain.push(n);
            via = st.alias[n as usize].as_ref().and_then(|x| x.via);
        }
        for n in 0..self.body.names.len() {
            if chain.contains(&(n as Name)) {
                continue;
            }
            // RFC-0090 is a rule about a buffer two names would see the write
            // through. A name the body owns read a value out, and a value
            // that owns no heap was copied out; neither aliases the place.
            if self.owned(n as Name) {
                continue;
            }
            let Some(x) = &st.alias[n] else {
                continue;
            };
            if x.root == a.root && overlaps(&x.path, &a.path) && st.dead[n].is_none() {
                st.dead[n] = Some((self.here, what.clone()));
            }
        }
    }

    /// A read of an alias whose place was written since: refused, at the
    /// write, in the checker's two-line form.
    fn alias_read(&self, st: &State, n: Name, what: &str) -> Result<(), Refusal> {
        let Some((l, place)) = &st.dead[n as usize] else {
            return Ok(());
        };
        let (s, here) = (self.src(n), self.here);
        // The way out is a value of its own, read where the alias was bound —
        // the place the alias reads, not the place the write named.
        let src = self.src_text(st, n);
        let at = self.body.names[n as usize].line;
        self.refuse_at(
            *l,
            menu(
                format!(
                    "`{place}` is written here while `{s}` still reads out of it\nline {here}: \
                     ... and `{s}` is {what} again here"
                ),
                vec![format!(
                    "`{src}.copy()` on line {at}, so `{s}` is a value of its own"
                )],
            ),
        )
    }

    /// A take of an alias: refused, because the place it reads still owns
    /// the buffer (RFC-0089 rule 2). Worded as `movecheck.rs` words each
    /// exit: the `consume` parameter, the `return`, the literal, the store.
    fn alias_take(&self, st: &State, n: Name, write_back: bool) -> Refusal {
        let (mut s, src, by) = (self.src(n), self.src_text(st, n), &self.by);
        // A temporary the reader never wrote is named by the PLACE it reads,
        // where that place is a spelling the reader can see: `sink(if c {
        // d.title } else { "" })` binds an unnamed borrow of `d.title`, and the
        // checker names the field the arm yielded (RFC-0125 §3 M3). A place
        // the algebra spelled — an element, another temporary — is quoted in
        // the sentence instead, below.
        if s.starts_with('@') && !src.starts_with('@') && !src.contains("[..]") {
            s = &src;
        }
        // Module state read whole: RFC-0013's own sentence, which names the
        // reason — the global lives for the whole module and nothing ever
        // drops it, so there is no owner to take from.
        if let Some(Alias {
            root: Root::G(g),
            path,
            ..
        }) = &st.alias[n as usize]
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
                // A return is the one of the three with a way out: the caller
                // releases what it is handed, so it is handed a copy.
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
            // A PROJECTION of module state is module state too, and the
            // checker says which fact refuses it rather than naming the place
            // (RFC-0125 §3 M3). The way out is the copy alone: there is no
            // owner to take the field from.
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
        // A loop variable is what the READER wrote, and the checker says so
        // rather than naming the place the element sits in — the alias table
        // has already decided the take is refused, and this is only the words
        // (RFC-0125 §3 M3, row 19).
        if let Some(of) = &self.body.names[n as usize].loop_var {
            return self.param_take(n, &BorrowKind::LoopVar { of: of.clone() });
        }
        // What the ROOT is, where the reader declared it: a `read` parameter,
        // a `modify` one, a loop variable. Both sentences are true — the place
        // owns the value AND the caller owns the place — and the checker says
        // the second, because the way out is written on the declaration and
        // not on the read (RFC-0125 §3 M3). A root this frame owns has no
        // capability to name, and keeps the place's own sentence below.
        //
        // A `drop` is not one of them: its two ways out are both about the
        // BINDING, and it words them below. Neither is a name the reader never
        // wrote — a temporary with no path of its own is quoted with the place
        // it reads, because a capability the reader can go and change is not
        // what it has to be told about.
        if by != "a `drop`" && !write_back && !s.starts_with('@') {
            // The NEAREST name on the chain, which is the one the checker asks
            // about: `p.name` inside `for p in ps` is read through `p`, and `p`
            // is a loop variable however the parameter behind it was declared.
            let mut m = st.alias[n as usize].as_ref().and_then(|a| a.via);
            let root = match &st.alias[n as usize] {
                Some(Alias {
                    root: Root::N(r), ..
                }) => Some(*r),
                _ => None,
            };
            while let Some(k) = m.or(root) {
                let info = &self.body.names[k as usize];
                if let Some(b) = &info.borrow_kind {
                    return self.param_take(n, b);
                }
                if let Some(of) = &info.loop_var {
                    return self.param_take(n, &BorrowKind::LoopVar { of: of.clone() });
                }
                if m.is_none() {
                    break;
                }
                m = st.alias[k as usize].as_ref().and_then(|a| a.via);
            }
        }
        // The name a refusal quotes is the reader's path where the lowering
        // minted this name for a read of a place, and the place is then the
        // subject rather than a second quotation of it: `b.xs` may not be
        // taken because it is read out of a place that owns it, which is how
        // the checker words the same refusal (RFC-0125 §3 M3, the corpus
        // slice). A name the READER bound is quoted with the place it reads,
        // because the two are different words.
        // The place is the SUBJECT where the lowering minted the name, and the
        // sentence is the checker's own for every name a reader wrote: the
        // place it reads out of is what the `.copy()` on the menu names, and
        // the sentence says what the name IS. A temporary the reader never
        // wrote is the one that keeps the place in the sentence, because its
        // own spelling says nothing (RFC-0125 §3 M3).
        let what = if s.starts_with('@') {
            format!("it is read out of `{src}`, a place that owns it")
        } else {
            "it is read out of a place that owns it".to_string()
        };
        let minted = self.body.names[n as usize].path.is_some();
        // A named binding a call takes: at the binding, as the checker words
        // it, so the `.copy()` on the menu lands where the read is. A minted
        // name has no binding a reader can look at, so this form has nowhere
        // to stand.
        if write_back && by.ends_with("(..)`") && !minted {
            let (here, at) = (self.here, self.body.names[n as usize].line);
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
        // A `drop` is the one exit whose sentence names no place, because its
        // ways out are both about the BINDING: take the value there, or let
        // the place that owns it release it (RFC-0089 rule 4, RFC-0125 §3 M3,
        // row 21).
        if by == "a `drop`" {
            // What the borrow IS, in `movecheck::Borrow::what`'s words: a
            // second name for a parameter where the alias reads one, and the
            // place otherwise. `examples/mustuse_abandoned.vyrn` is the
            // program that needed the first — `let ops = self.ops` inside a
            // `read self` method.
            let kind = match &st.alias[n as usize] {
                Some(Alias {
                    root: Root::N(m), ..
                }) => self.body.names[*m as usize]
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
                            "delete the `drop` — the place that owns it releases it (RFC-0089 \
                             rule 4)"
                                .to_string(),
                        ],
                    ),
                )
                .unwrap_err();
        }
        // The export's own sentence, as [`Kernel::param_take`] gives it one
        // borrow over: the caller across this boundary is JS and `wasi-min.js`
        // frees every String an export hands back (RFC-0012 M2, RFC-0089 M3b),
        // so an export owns its result whatever the borrow is — a parameter
        // there, a read of a place here — and the copy is the one way out. It
        // was on one of the two paths and not the other, which is the same
        // accident `movecheck::refuse_return` exists to stop (RFC-0125 §3 M3,
        // row 17).
        let msg = if by == "a `return`" && self.body.export {
            format!(
                "`{s}` may not be returned from an exported function — {what}, and the JS \
                 caller releases what it is handed"
            )
        } else if by == "a `return`" {
            // The same clause the parameter's own return sentence carries
            // ([`Kernel::param_take`]) and the checker's: what makes the read
            // wrong here is the exit, not the read.
            format!("`{s}` may not be returned — {what}, and a return is owned")
        } else {
            format!("{} — {what}", self.may_not(s))
        };
        let fixes = if by == "a `return`" && self.body.export {
            vec![format!(
                "`{s}.copy()` — an `export extern fn` owns its result"
            )]
        } else {
            self.place_fixes(st, n)
        };
        self.refuse_at::<()>(self.here, menu(msg, fixes))
            .unwrap_err()
    }

    /// The ways out of a take of a place read, in `movecheck::Borrow::fixes`'s
    /// words: the take where the root is one this frame owns, and the copy
    /// always. RFC-0093 puts the take first because it is the answer that
    /// allocates nothing — a field of a root nobody borrowed may be moved out,
    /// and the field is dead afterwards. A borrowed root and module state are
    /// `.copy()` alone, which is exactly where the take would be refused in
    /// turn. Empty for a name the reader bound, whose refusal quotes the place
    /// rather than being it.
    fn place_fixes(&self, st: &State, n: Name) -> Vec<String> {
        // A name the READER bound is its own spelling: the copy is the one way
        // out, because `consume t` takes nothing out of a place. A temporary
        // with neither a path nor a spelling has no menu at all — its refusal
        // quotes the place instead (RFC-0125 §3 M3).
        let own_name = self.src(n).to_string();
        let read = self.src_text(st, n);
        let path = match &self.body.names[n as usize].path {
            Some(p) => p.clone(),
            // The place an unnamed temporary reads is its spelling here too,
            // so the ways out land on what the reader wrote.
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
        // An ELEMENT has no take — `check_take` refuses one — so where a
        // declared `consume` parameter is the taker the menu names the two
        // spellings that exist for it instead of a prefix take
        // (`movecheck::refuse_projected_arg`). A path that reaches an element
        // is the one `place_path` cannot spell, and it is the one this pass
        // spelled with brackets.
        let root = match &st.alias[n as usize] {
            Some(Alias {
                root: Root::N(m), ..
            }) if self.body.names[n as usize].path.is_some() || self.src(n).starts_with('@') => {
                self.src(*m)
            }
            _ => path.as_str(),
        };
        if self.takes == Taker::Declared && path.contains('[') {
            return vec![
                format!("`{path}.copy()` — the callee owns its copy"),
                format!(
                    "`{root}.swapRemove(..)` returns the element and leaves the container \
                     one shorter"
                ),
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

    /// Whether the place this name reads out of has a root THIS frame owns, so
    /// a prefix take of the path is an answer rather than a second refusal.
    fn root_owns(&self, st: &State, n: Name) -> bool {
        matches!(
            &st.alias[n as usize],
            Some(Alias { root: Root::N(m), .. })
                if self.body.names[*m as usize].borrow_kind.is_none()
        )
    }

    /// `<name> may not be <verb> <taker>`, stated once for every refusal that
    /// names one. The verb is the taker's ([`Taker`]) and the words are
    /// `movecheck`'s: a declared `consume` parameter is passed to, a builtin's
    /// sink and a store are stored into, a variant constructor is put into.
    fn may_not(&self, s: &str) -> String {
        let by = &self.by;
        if by == "a literal" {
            // A part of a RECORD literal goes into a field, and the checker
            // names it. "The literal" is what is left where the core has no
            // field names — an array, a map, a variant (RFC-0125 §3 M3, row
            // 07). The same list [`Kernel::gone`] reads for a rule-1 move.
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
        if !by.ends_with("(..)`") {
            return format!("`{s}` may not be stored into {by}");
        }
        match self.takes {
            Taker::Declared => {
                format!("`{s}` may not be passed to a `consume` parameter via {by}")
            }
            Taker::Constructs => format!("`{s}` may not be put into {by}"),
            Taker::Stores => format!("`{s}` may not be stored into {by}"),
        }
    }

    /// A refusal at the statement being judged.
    fn refuse<T>(&self, msg: String) -> Result<T, Refusal> {
        self.refuse_at(self.here, msg)
    }

    fn refuse_at<T>(&self, line: usize, msg: String) -> Result<T, Refusal> {
        Err(Refusal {
            message: msg,
            line,
            file: self.body.file.clone(),
            body: self.body.name.clone(),
        })
    }

    /// A name used after it was consumed, in the checker's two wordings: a
    /// `consume` parameter took it ("`s` is used here but was already
    /// consumed by `take(..)` on line 7"), or something else did ("`s` was
    /// moved here into the binding `t` / line 4: ... and `s` is used again
    /// here", at the move).
    fn used_after(&self, st: &State, n: Name, what: &str) -> Refusal {
        self.used_after_at(st, n, what, "")
    }

    /// The same, for a read of a sub-place. The `consume` wording names the
    /// PATH the read spells (`x.id`), as `movecheck::check_read` does; the
    /// move wording names the storage that moved, as it does too.
    fn used_after_at(&self, st: &State, n: Name, what: &str, path: &str) -> Refusal {
        let s = self.src(n);
        let read = format!("{s}{}", path.replace(".[]", "[..]"));
        let here = self.here;
        // A `drop` a reader wrote takes the value as a `consume` parameter
        // does, and the checker says so in the same sentence (row 06). The
        // note under it is about a READ, so a second `drop` does not print
        // it (row 20).
        let note = if what == "dropped" {
            ""
        } else {
            "\n  (a `consume` parameter takes ownership; the value can't be used afterward)"
        };
        let r = match &st.taker[n as usize] {
            // The sentence turns on HOW the taker took, which the checker asks
            // as "does this consumption carry a `.copy()` menu?" — a parameter
            // the author declared `consume`, and a `drop`, carry none, and
            // everything else does (RFC-0125 §3 M3, rows 06 and 07). The test
            // used to be the SHAPE of the taker's words, which reads a
            // builtin's sink (`push(..)`, `fromArray(..)`) as a declared
            // parameter and gave four programs of the corpus rule 1's sentence
            // where the checker gives the move's two lines. A LINEAR value is
            // the exception the other way: `close(s)` is a builtin and the
            // must-use walk owns it, so the checker words a use after it as a
            // `consume` parameter's — which is the one builtin `sinks` answers
            // `false` for ([`crate::core::NameInfo::linear`]).
            Some((l, by, t))
                if *t == Taker::Declared
                    || by == "`drop`"
                    || self.body.names[n as usize].linear =>
            {
                self.refuse_at::<()>(
                    here,
                    format!(
                        "`{read}` is {what} here but was already consumed by {by} on line {l}{note}"
                    ),
                )
            }
            Some((l, by, _)) if !by.is_empty() => self.refuse_at::<()>(
                *l,
                menu(
                    format!(
                        "`{s}` was moved here into {by}\nline {here}: ... and `{s}` is {what} \
                         again here"
                    ),
                    vec![format!("`{s}.copy()` if both sides need a value")],
                ),
            ),
            _ => self.refuse_at::<()>(here, format!("`{s}` is {what} here after it was released")),
        };
        r.unwrap_err()
    }

    /// The line a hole in `n` at `path` was taken on.
    fn hole_line(&self, st: &State, n: Name, path: &str) -> usize {
        st.taken_at
            .iter()
            .rev()
            .find(|(h, p, _)| *h == n && p == path)
            .map(|(_, _, l)| *l)
            .unwrap_or(self.body.names[n as usize].line)
    }

    /// Every name in `names` that is still held is a leak at this scope's
    /// end — refused when judging, recorded and released when placing.
    fn scope_end(
        &mut self,
        st: &mut State,
        names: &[Name],
        exit: Exit,
        site: usize,
    ) -> Result<(), Refusal> {
        self.ending.set(true);
        let mark = self.missing.len();
        let out = self.scope_end_inner(st, names, exit, site);
        // Newest binding first, which is the order a frame is unwound in
        // (RFC-0125 §3 M3, the walk's deletion). `names` is CREATION order at
        // every one of the three exits — `bound_here` for a block,
        // `bound_inside` for a loop edge, `all_names` for a return — so
        // reversing what this scope end pushed is that order: a block's exit
        // runs its own frame alone; a `break` or a `continue` unwinds from the
        // loop body inward, and an inner frame's names are created after the
        // frame outside it; a return walks every frame, and a parameter has
        // the lowest name index of all, so it is released LAST, which is what
        // RFC-0114 says an owned `consume` parameter does.
        self.missing[mark..].reverse();
        self.ending.set(false);
        out
    }

    fn scope_end_inner(
        &mut self,
        st: &mut State,
        names: &[Name],
        exit: Exit,
        site: usize,
    ) -> Result<(), Refusal> {
        for n in names {
            if self.owned(*n) && st.own[*n as usize] == Own::Static {
                self.gone(st, *n);
            }
            // A name that owes no release leaves its scope owing nothing: the
            // ownership state ends with the scope and no row is placed.
            if self.owned(*n) && !self.releases(*n) && st.own[*n as usize] == Own::Held {
                self.unbind(st, *n);
                continue;
            }
            if self.owned(*n) && st.own[*n as usize] == Own::Held {
                // A placed row releases the whole value minus the holes it
                // carries (RFC-0125 M3): the row is told the holes this
                // state has here, which may differ from the binding's own
                // set on another path.
                if self.mode == Mode::Place {
                    let holes = self.holes_owned(st, *n);
                    // An exit INSIDE an arm is one arm's exit, and an exit row
                    // is keyed by the exit alone — so a row read off one arm
                    // would be emitted on the arm beside it, which took the
                    // name (`std/html.vyrn`'s `keyed`). Both tables that are
                    // keyed by the ARM say it once: the binder's own row for a
                    // binder, and RFC-0114 Rule N's edge for a name the frame
                    // bound outside (RFC-0125 §3 M3, row 17).
                    //
                    // The edge table takes only what it can carry, which is
                    // the same rule [`Kernel::equalize`] states one level
                    // down. It names a row by its SPELLING, so a temporary the
                    // lowering minted cannot go in it — `std/hash.vyrn`'s
                    // `sha1Hex` holds one at both arms of a returned `match`,
                    // and the rebuild could not lower it at all. And an edge
                    // row releases the WHOLE value, so a name with holes
                    // cannot go in it either — `graphql.vyrn`'s `gqlResolve`
                    // holds an `arg` whose `.err` a path before the `match`
                    // took, and the edge drop freed it around a field no arm
                    // had taken. Both take the exit's own row instead, like
                    // any other local of the frame: one rule for a frame's
                    // locals, stated once (RFC-0125 §3 M3, the walk's
                    // deletion). Neither is a name one arm takes and another
                    // holds, which is what the edge table is for.
                    let (exit, site, kind) = match self.arms.last() {
                        Some((s, arm, binds)) if *s != 0 && binds.contains(n) => {
                            (Exit::Block, *s, MissingKind::ArmBinder { arm: *arm })
                        }
                        Some((s, arm, _))
                            if *s != 0
                                && !self.body.names[*n as usize].source.starts_with('@')
                                && self.body.names[*n as usize].holes.is_empty() =>
                        {
                            (exit, *s, MissingKind::Edge { edge: *arm })
                        }
                        _ => (exit, site, MissingKind::Exit),
                    };
                    self.missing.push(Missing {
                        exit,
                        site,
                        name: *n,
                        kind,
                        holes,
                    });
                    self.gone(st, *n);
                    continue;
                }
                return self.refuse(format!(
                    "{} is still held at {} — no release is placed for it",
                    self.info(*n),
                    match exit {
                        Exit::Block => "the end of its scope".to_string(),
                        Exit::Return => "a `return`".to_string(),
                        Exit::Try => "a `?`".to_string(),
                        Exit::Break => "a `break`".to_string(),
                        Exit::Continue => "a `continue`".to_string(),
                        Exit::Scrutinee => "a scrutinee".to_string(),
                    }
                ));
            }
        }
        Ok(())
    }

    /// A release of `n` that walks around `holes`. The set must be the holes
    /// the state has: a place the walk reaches after a take left it is a
    /// double free; a place the walk skips while it is still held is a leak,
    /// which a placed row (`at`) repairs by taking the state's set.
    fn drop(
        &mut self,
        st: &mut State,
        n: Name,
        holes: &[String],
        at: Option<(Exit, usize)>,
    ) -> Result<(), Refusal> {
        if !self.owned(n) {
            // A release IS a take, so a borrow released here is RFC-0089
            // rule 4 refused: the place that owns the value releases it, and
            // this frame is not that place. Worded as the checker words a
            // `drop` (RFC-0125 §3 M3, the census, rows 21 and 29).
            //
            // A `for x in consume xs` is the exception, and it is the reason
            // rows 10, 11 and 29 could not leave the checker: the container's
            // release is where the loop's take lands, and a reader who wrote
            // the loop was told about a `drop` no program of theirs contains.
            // The form is on the name the core bound the container to, as it
            // is at the `let` ([`crate::core::NameInfo::for_consume`]).
            if st.alias[n as usize].is_some() {
                let form = if self.body.names[n as usize].for_consume {
                    "the `for .. in consume` loop"
                } else {
                    "a `drop`"
                };
                let by = std::mem::replace(&mut self.by, form.to_string());
                let r = self.alias_take(st, n, false);
                self.by = by;
                return Err(r);
            }
            return self.refuse(format!(
                "{} is released although the body does not own it",
                self.info(n)
            ));
        }
        // A release of a value that owns no heap frees nothing. The plan
        // places such a row where its edge table wants one and every engine
        // reads it as nothing; the ownership state ends here all the same.
        if !self.releases(n) {
            self.unbind(st, n);
            return Ok(());
        }
        if st.own[n as usize] == Own::Gone {
            // A `drop` a reader wrote is worded as the reader wrote it; a
            // release this pass placed is worded as a release (row 20).
            let what = if self.by == "`drop`" {
                "dropped"
            } else {
                "released"
            };
            return Err(self.used_after(st, n, what));
        }
        if st.own[n as usize] == Own::Held {
            let state = self.holes_owned(st, n);
            // Every place that left must be under a hole the row skips.
            if let Some(h) = state.iter().find(|h| !holes.iter().any(|r| covers(r, h))) {
                // A `drop` a reader WROTE is worded as the reader wrote it,
                // with the two ways out (RFC-0125 §3 M3, row 22): `drop`
                // reclaims storage by type and cannot be told to skip the
                // places a take handed away, so the spelling is refused and
                // the menu names the write-back and the deletion. A release
                // this pass placed has no spelling in the program, so it is
                // worded as a release — the same distinction `self.by` draws
                // one refusal above.
                if self.by == "`drop`" {
                    let (s, l) = (self.src(n), self.hole_line(st, n, h));
                    return self.refuse(menu(
                        format!(
                            "`{s}` may not be dropped — `{s}{h}` was taken out of it on \
                             line {l}, and `drop` releases the whole binding"
                        ),
                        vec![
                            format!(
                                "write `{s}{h}` back before the `drop`, so the binding is \
                                 whole again"
                            ),
                            "delete the `drop` — the parts still here are released when the \
                             block exits"
                                .to_string(),
                        ],
                    ));
                }
                return self.refuse(format!(
                    "{} is released whole although a `consume` took `{h}` out of it",
                    self.info(n)
                ));
            }
            // Every hole the row skips must be under a place that left.
            let left: Vec<&String> = holes
                .iter()
                .filter(|r| !state.iter().any(|h| covers(h, r)))
                .collect();
            if let Some(r) = left.first() {
                match (self.mode, at) {
                    (Mode::Place, Some((exit, site))) => self.missing.push(Missing {
                        exit,
                        site,
                        name: n,
                        kind: MissingKind::Exit,
                        holes: state,
                    }),
                    _ => {
                        return self.refuse(format!(
                            "{} is released around `{r}` on a path that did not take it",
                            self.info(n)
                        ))
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

    /// The same, told the path under the name that is read, for the wording.
    fn read_at(&self, st: &State, v: &Val, path: &str) -> Result<(), Refusal> {
        if let Val::Name(n) = v {
            if self.owned(*n) && self.used_up(st, *n) {
                return Err(self.used_after_at(st, *n, "used", path));
            }
            self.alias_read(st, *n, "used")?;
        }
        Ok(())
    }

    /// RFC-0037's capture rule: a closure that outlives the call it is
    /// written at may not capture a borrow, because the borrow's owner is
    /// this frame and the closure leaves it (RFC-0125 §3 M3, row 24).
    ///
    /// What the borrow IS comes from the same two places every other refusal
    /// reads it from: the kind the core minted where it has one, and the
    /// alias table's place where it does not — the reading `alias_take`'s
    /// `drop` branch makes, one rule over.
    fn escaping_capture(&self, st: &State, caps: &[Name], line: usize) -> Result<(), Refusal> {
        for c in caps {
            if !self.borrowed(*c) {
                continue;
            }
            let s = self.src(*c);
            let (what, fixes) = match &self.body.names[*c as usize].borrow_kind {
                Some(b) => (b.what(s), b.fixes(s)),
                None => (
                    match &st.alias[*c as usize] {
                        Some(Alias {
                            root: Root::N(m), ..
                        }) => self.body.names[*m as usize]
                            .borrow_kind
                            .as_ref()
                            .map(|b| b.what(s)),
                        _ => None,
                    }
                    .unwrap_or_else(|| "read out of a place that owns it".to_string()),
                    Vec::new(),
                ),
            };
            return self.refuse_at(
                line,
                menu(
                    format!(
                        "`{s}` may not be captured by a closure that outlives this call \
                         — it is {what}"
                    ),
                    fixes,
                ),
            );
        }
        Ok(())
    }

    /// A take of a `read` or `modify` parameter, of a second name for one,
    /// or of a lambda frame's capture: refused, because somebody else owns
    /// it (RFC-0089 rule 2, RFC-0037). Neither has a place to be an alias
    /// of, which is why the alias table does not see them (RFC-0125 §3 M3,
    /// the census).
    fn param_take(&self, n: Name, b: &BorrowKind) -> Refusal {
        let (s, by) = (self.src(n), &self.by);
        let what = b.what(s);
        let msg = if by == "a `return`" && matches!(b, BorrowKind::Capture) {
            format!(
                "`{s}` may not be returned from a closure — it is a captured \
                 binding, and the closure's result is its caller's"
            )
        } else if by == "a `return`" && self.body.export {
            // RFC-0012 M2: the caller is JS and it releases what it is
            // handed, so an export owns its result or it does not compile.
            format!(
                "`{s}` may not be returned from an exported function — it is {what}, \
                 and the JS caller releases what it is handed"
            )
        } else if by == "a `return`" {
            format!("`{s}` may not be returned — it is {what}, and a return is owned")
        } else {
            format!("{} — it is {what}", self.may_not(s))
        };
        // The ways out, as `movecheck::Borrow::fixes` and
        // `movecheck::MoveCheck::fixes_here` name them. An `export extern fn`
        // has one: its JS caller releases the String the call returns, so the
        // signature refuses `consume` and only a copy is left (RFC-0089 M3b).
        let capture = matches!(b, BorrowKind::Capture);
        // A constructor names one way out: the value it makes owns what it is
        // given, so the copy is the answer and a `consume` on the parameter is
        // not (RFC-0125 §3 M3, row 19).
        let fixes = if self.takes == Taker::Constructs && !capture {
            vec![format!("`{s}.copy()` if the value should own it")]
        } else if by == "a `return`" && capture {
            vec![format!("`{s}.copy()` if the caller needs its own value")]
        } else if capture {
            Vec::new()
        } else if self.body.export && by == "a `return`" {
            vec![format!(
                "`{s}.copy()` — an `export extern fn` owns its result"
            )]
        } else if self.body.export {
            vec![format!(
                "`{s}.copy()` — an `export extern fn` may not take ownership of a String its \
                 JS caller releases"
            )]
        } else {
            b.fixes(s)
        };
        self.refuse_at::<()>(self.here, menu(msg, fixes))
            .unwrap_err()
    }

    /// A take of a name: it must be held, and it is gone afterwards. An
    /// alias is never taken.
    fn take(&self, st: &mut State, v: &Val) -> Result<(), Refusal> {
        self.take_arg(st, v, false, false)
    }

    /// A take, told whether it is the receiver of a rebuilding builtin
    /// (`out.push(v)`): that one take changes no owner, because the store
    /// after the call puts the value back where it came from, so a `modify`
    /// parameter may be its subject. The core states the exception
    /// ([`crate::core::Rhs::Call::write_back`]) and the rule under it is
    /// `prelude::rebuilds`, which `movecheck::sinks` reads too.
    fn take_arg(
        &self,
        st: &mut State,
        v: &Val,
        write_back: bool,
        consume: bool,
    ) -> Result<(), Refusal> {
        if let Val::Name(n) = v {
            if !write_back {
                let i = &self.body.names[*n as usize];
                if let Some(b) = &i.borrow_kind {
                    // RFC-0075 M1: a must-use parameter is the callee's to
                    // hand on, whatever its capability says, so this take is
                    // not the caller's value leaving ([`NameInfo::must_use_param`]).
                    if self.borrowed(*n) && !i.must_use_param {
                        return Err(self.param_take(*n, b));
                    }
                }
            }
            if st.alias[*n as usize].is_some() {
                self.alias_read(st, *n, "used")?;
                if self.moves(*n, consume) {
                    return Err(self.alias_take(st, *n, write_back));
                }
                return Ok(());
            }
            if self.owned(*n) {
                if self.used_up(st, *n) {
                    return Err(self.used_after(st, *n, "used"));
                }
                if let Some((_, path)) = st.holes.iter().find(|(h, _)| h == n) {
                    let s = self.src(*n);
                    let (here, l) = (self.here, self.hole_line(st, *n, path));
                    return self.refuse_at(
                        l,
                        menu(
                            format!(
                                "`{s}{path}` was taken out of `{s}` here\nline {here}: ... and \
                                 `{s}` is used as a whole here, with the hole still in it"
                            ),
                            vec![
                                format!(
                                    "`{s}{path}.copy()` on line {l} if `{s}` is still needed whole"
                                ),
                                format!("write `{s}{path}` back before this line"),
                            ],
                        ),
                    );
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
                let s = self.src(n);
                let (here, l) = (self.here, self.hole_line(st, n, h));
                // Both lines name the STORAGE that moved, not the longer path
                // that reads it — the rule `used_after_at` states and the one
                // `movecheck::check_use` states. A read of `d.a.byteLength`
                // after `consume d.a` was the one refusal in the tree that
                // broke it, and it dropped the menu with it: the reader wanted
                // a value on both sides (RFC-0125 §3 M3, row 07).
                // Both lines name the STORAGE that moved, not the longer path
                // that reads it — the rule `used_after_at` states and the one
                // `movecheck::check_use` states. A read of `d.a.byteLength`
                // after `consume d.a` was the one refusal in the tree that broke
                // it, and it dropped the menu with it: the reader wanted a value
                // on both sides (RFC-0125 §3 M3, row 07).
                return self.refuse_at(
                    l,
                    menu(
                        format!(
                            "`{s}{h}` was moved here into `consume`\n\
                             line {here}: ... and `{s}{h}` is used again here"
                        ),
                        vec![format!("`{s}{h}.copy()` if both sides need a value")],
                    ),
                );
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

    /// A take out of a sub-place: the root keeps a hole there, and every
    /// alias of the place ends.
    fn take_place(&self, st: &mut State, p: &Place) -> Result<(), Refusal> {
        self.place(st, p)?;
        self.wrote(st, p, &self.place_text(p));
        if let Some((n, path)) = root_of(p) {
            if self.owned(n) && !path.is_empty() {
                st.taken_at.push((n, path.clone(), self.here));
                st.holes.push((n, path));
                st.holes.sort();
            }
        }
        Ok(())
    }

    /// A store into a sub-place fills the hole there, and anything under it.
    /// A store under a hole writes into what left. Every alias of the place
    /// ends.
    /// Record a store whose place this path still holds — RFC-0125 §3 M3, the
    /// store slice.
    ///
    /// The row is keyed by the store's own node and by nothing else, so a
    /// store this pass made up — a global's initializer, a desugar's
    /// temporary, the block RFC-0091 M2's `place at` rewrite builds — states
    /// no key and no reader could find the row by one.
    fn owe_store(&mut self, site: &crate::core::Site) {
        if self.mode != Mode::Place {
            return;
        }
        let crate::core::Site::Node(at) = site else {
            return;
        };
        if std::env::var("VYRN_KERNEL_TRACE").is_ok() {
            eprintln!("owe-store: {} line {} site {at}", self.body.name, self.here);
        }
        self.missing.push(Missing {
            exit: Exit::Block,
            site: *at,
            name: 0,
            kind: MissingKind::Store,
            holes: Vec::new(),
        });
    }

    fn store_place(&self, st: &mut State, p: &Place) -> Result<(), Refusal> {
        self.indices(st, p)?;
        self.wrote(st, p, &self.place_text(p));
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
            let s = self.src(n);
            let (here, l) = (self.here, self.hole_line(st, n, h));
            return self.refuse_at(
                l,
                format!(
                    "`{s}{h}` was moved here into `consume`\nline {here}: ... and `{s}{path}` \
                     is written here, under the hole"
                ),
            );
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
                args,
                write_back,
                kind,
                ..
            } => {
                // Reads first, takes after: the call sees every argument
                // before it owns any, so a receiver handed back through the
                // result (`dup.append(dup)`) is read and taken by one call.
                for (v, cap) in args {
                    if !matches!(cap, Capability::Consume) {
                        self.read(st, v)?;
                    }
                }
                for (i, (v, cap)) in args.iter().enumerate() {
                    if matches!(cap, Capability::Consume) {
                        // Only a DECLARED `consume` parameter takes a value
                        // that owns no heap: it is the author's word that the
                        // callee owns what it is handed (RFC-0089 rule 1). A
                        // builtin sink and a variant constructor store the
                        // value, and storing one that owns no heap copies it.
                        self.take_arg(st, v, *write_back && i == 0, kind.declared())?;
                    }
                }
                // A `modify` argument does NOT end the aliases of what it is
                // handed, and the census measured why (RFC-0125 §3 M3).
                // Ending them refuses `freeNode` in `tree.vyrn`,
                // `linkedlist.vyrn` and `freelist.vyrn`: each reads
                // `t[h].left` — an `Option<Handle<T>>`, which owns heap
                // because a wide payload travels boxed — and then calls
                // `remove(t, h)`, which shuffles index arrays and never
                // touches the payload the read points into. The rule needs
                // to know WHICH place a callee writes, and that is the
                // per-argument retention over the call graph the deletion
                // track still owes.
                Ok(())
            }
            Rhs::Prim(_, vs, _) => {
                for v in vs {
                    self.read(st, v)?;
                }
                Ok(())
            }
            Rhs::Make(_, vs) => {
                // A record literal takes each part into a FIELD, and the
                // refusal names it — "a literal" is where the value went for a
                // reader who did not write one (RFC-0125 §3 M3, row 07). The
                // words are the bound name's, because the core keeps no
                // statement kinds; empty for an array, a map and a variant,
                // whose parts the checker does not name either.
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
    /// it, at no site the plan knows.
    fn stmts(&mut self, stmts: &[St], st: &mut State) -> Result<(), Refusal> {
        self.stmts_at(stmts, st, 0)
    }

    /// Record a refusal and carry on ([`Kernel::refusals`]).
    fn also(&mut self, r: Result<(), Refusal>) {
        if let Err(r) = r {
            self.refusals.push(r);
        }
    }

    fn stmts_at(&mut self, stmts: &[St], st: &mut State, site: usize) -> Result<(), Refusal> {
        let mut bound_here: Vec<Name> = Vec::new();
        for s in stmts {
            if st.ended {
                // Code after a return, break or continue: the checker has
                // already refused what it can; nothing here runs.
                break;
            }
            if !self.recover {
                self.stmt(s, st, &mut bound_here)?;
                continue;
            }
            // A refused statement is UNDONE, and the next one is judged in
            // the state before it. The half-judged state is not a state the
            // program ever has: a rebuilding call that refused never handed
            // its receiver back, and a temporary it minted was never bound,
            // so the walk that carried on would refuse the receiver as moved
            // and the temporary as used after a release — sentences about
            // machinery, not about the program (RFC-0125 §3 M3).
            let before = st.clone();
            let bound = bound_here.len();
            let missing = self.missing.len();
            let one = self.stmt(s, st, &mut bound_here);
            if one.is_err() {
                *st = before;
                bound_here.truncate(bound);
                self.missing.truncate(missing);
                // What the statement BOUND still stands, because the reader
                // wrote a `let` and every statement after it names what the
                // `let` names. Undoing that too would refuse the next store
                // as a use of a name this body never bound.
                if let St::Let(n, _) = s {
                    if self.owned(*n) {
                        st.own[*n as usize] = Own::Held;
                        bound_here.push(*n);
                    }
                }
            }
            self.also(one);
        }
        if !st.ended {
            self.scope_end(st, &bound_here, Exit::Block, site)?;
        }
        Ok(())
    }

    /// What a right-hand side takes its operands with, in the checker's
    /// words: the binding it is bound to, the call, the `consume`, a literal.
    fn by_of(&self, rhs: &Rhs, bound: Option<Name>) -> String {
        match rhs {
            Rhs::Val(_) => match bound {
                Some(n) if !self.src(n).starts_with('@') => {
                    format!("the binding `{}`", self.src(n))
                }
                // A `for x in consume xs` takes the container into a temporary
                // of its own, and the reader wrote the loop, not the temporary
                // (RFC-0125 §3 M3, row 07).
                Some(n) if self.body.names[n as usize].for_consume => {
                    "the `for .. in consume` loop".to_string()
                }
                _ => "a value".to_string(),
            },
            Rhs::Call { callee, .. } => {
                format!("`{}(..)`", callee.trim_start_matches('@'))
            }
            Rhs::Take(_) => "`consume`".to_string(),
            Rhs::Make(..) => "a literal".to_string(),
            Rhs::Read(_) | Rhs::Prim(..) => String::new(),
        }
    }

    fn stmt(&mut self, s: &St, st: &mut State, bound_here: &mut Vec<Name>) -> Result<(), Refusal> {
        // The line and the taker every consumption in this statement is
        // recorded with (RFC-0125 M3, third slice).
        self.how = match s {
            St::Return { .. } => TookHow::Return,
            St::Drop(_, _, line) if *line > 0 => TookHow::Drop,
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
                self.here = self.body.names[*n as usize].line;
                self.by = self.by_of(rhs, Some(*n));
                self.takes = taker_of(rhs);
                self.made = self.body.names[*n as usize].fields.clone();
                self.rebound(*n);
                self.released.borrow_mut()[*n as usize] = None;
            }
            St::Store { place, line, .. } => {
                if let Place::Name(n) = place {
                    self.rebound(*n);
                    self.released.borrow_mut()[*n as usize] = None;
                }
                self.here = *line;
                self.takes = Taker::Stores;
                self.by = match place {
                    Place::Name(n) if !self.src(*n).starts_with('@') => {
                        format!("the binding `{}`", self.src(*n))
                    }
                    Place::Field(_, f) => format!("the field `{f}`"),
                    // The checker names the CONTAINER an element or a key store
                    // writes into, and module state by the words that say what
                    // it is (RFC-0125 §3 M3). "A store" was what was left when
                    // the place was neither a bare name nor a field, and it
                    // names nothing a reader can go and look at.
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
                self.takes = Taker::Stores;
            }
            St::Do { rhs, line, .. } => {
                self.here = *line;
                self.takes = taker_of(rhs);
                self.by = self.by_of(rhs, None);
            }
            St::Switch { line, .. } => {
                self.here = *line;
                self.by = "a `match`".to_string();
            }
            // A `drop` a reader WROTE is a statement with a line, and the
            // taker it records is the word the reader used. A release this
            // pass placed has neither: it stands at the binding, and nothing
            // took the value (RFC-0125 §3 M3, rows 06, 20 and 21).
            St::Drop(n, _, line) if *line > 0 => {
                self.here = *line;
                self.by = "`drop`".to_string();
            }
            // A row the placement walk placed: this is what "reclaimed at
            // block exit" means, and the row carries the holes it walks
            // around (RFC-0093 M2).
            St::Row { name: n, holes, .. } => {
                self.here = self.body.names[*n as usize].line;
                self.by = String::new();
                self.released.borrow_mut()[*n as usize] = Some(holes.clone());
            }
            St::Drop(n, ..) => {
                self.here = self.body.names[*n as usize].line;
                self.by = String::new();
            }
            _ => {}
        }
        match s {
            St::Let(n, rhs) => {
                // A LITERAL owns no heap yet: `let mut acc = ""` names the
                // data segment until a store gives it a buffer. `Static` is a
                // statement about a RELEASE — there is none until then — so a
                // name that owes none is never `Static`, only held or gone.
                //
                // A `Make` of literals is NOT one, and the walk's deletion is
                // what showed it (RFC-0125 §3 M3). `[4, 5]` calls the
                // runtime's constructor and the buffer it hands back is this
                // frame's; the core says so where it binds the name — an
                // array literal is no `Expr::Str` and
                // [`crate::core::Builder::owned_binding`] never called it
                // literal — and the plan's row freed it on every engine. The
                // kernel alone said `Static`, so with the walk's row gone
                // nothing released a local array at all: 34 corpus programs
                // began to leak, `consume_handover`'s `b` the smallest.
                //
                // A RECORD of literals is no exception, and the corpus says
                // which way: `Book { title: "Dune", body: () -> loadBody(1) }`
                // holds a thunk the construction allocated
                // (`examples/lazyfield.vyrn`), so a rule that spared a record
                // spared that too. What the whole `Make` arm was buying is one
                // refusal the kernel now gives as well as the checker —
                // `r22_drop_with_a_hole`'s `drop p` after a take of `p.name`
                // — and the census records it there.
                let is_static = self.releases(*n) && matches!(rhs, Rhs::Val(Val::Lit(_)));
                // RFC-0037 at a capture: a closure that OUTLIVES the call it
                // is written at may not hold a borrow. A lambda whose
                // parameter provably only borrows it dies with the call and
                // captures freely — that is the common case and the core says
                // which ([`crate::core::NameInfo::closure_escapes`]). One that
                // is stored, returned, or handed to a parameter that may keep
                // it is a value under RFC-0037's defunctionalization, and a
                // borrow inside one has no lifetime to stand on (RFC-0125 §3
                // M3, row 24).
                if matches!(rhs, Rhs::Prim(crate::core::Op::Closure, ..)) {
                    let i = &self.body.names[*n as usize];
                    if let Some(reads) = i.closure_reads.clone() {
                        self.escaping_capture(st, &reads, i.line)?;
                    }
                }
                // An alias: a borrow read out of a place, or a second name
                // for a borrow. What it reads is kept, and a second name for
                // a borrow is not a take of it.
                st.dead[*n as usize] = None;
                st.alias[*n as usize] = None;
                match rhs {
                    Rhs::Read(p) if self.borrowed(*n) => {
                        st.alias[*n as usize] = Some(self.src_of(st, p));
                    }
                    // A read of module state is an alias of it whatever it
                    // holds: RFC-0013 is a rule about the LIFETIME of a
                    // global — it lives for the whole module and nothing ever
                    // drops it — and not about heap, so a `consume` parameter
                    // may not take a heapless global either.
                    Rhs::Read(p @ Place::Global(_)) if self.owned(*n) => {
                        st.alias[*n as usize] = Some(self.src_of(st, p));
                    }
                    Rhs::Val(Val::Name(m)) if self.borrowed(*n) && self.borrowed(*m) => {
                        self.read(st, &Val::Name(*m))?;
                        st.alias[*n as usize] = Some(self.src_of(st, &Place::Name(*m)));
                        return Ok(());
                    }
                    _ => {}
                }
                self.rhs(st, rhs)?;
                if self.owned(*n) {
                    st.own[*n as usize] = if is_static { Own::Static } else { Own::Held };
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
                    if self.borrowed(*n) && st.alias[*m as usize].is_some() {
                        self.read(st, value)?;
                        self.wrote(st, place, self.src(*n));
                        st.alias[*n as usize] = st.alias[*m as usize].clone();
                        st.dead[*n as usize] = None;
                        return Ok(());
                    }
                }
                // The write-back of RFC-0082's place desugar (`t.xs[k] = v`
                // reads `t.xs` into a temporary, stores, and writes it back):
                // the alias goes back into the very place it reads, so
                // nothing changes owner. The alias ends with it.
                if let Val::Name(m) = value {
                    let into = self.src_of(st, place);
                    let back = st.alias[*m as usize]
                        .as_ref()
                        .is_some_and(|a| a.root == into.root && a.path == into.path);
                    if back {
                        self.read(st, value)?;
                        self.wrote(st, place, &self.place_text(place));
                        st.dead[*m as usize] = Some((self.here, self.place_text(place)));
                        return Ok(());
                    }
                }
                // Read before the take, which ends the temporary: a store
                // gives its target the state the value has, so `b = Body {
                // nodes: [] }` leaves `b` `Static` exactly as the same
                // expression does at a `let`. The rule is the `let`'s, stated
                // once (RFC-0125 §3 M3, the default slice); without it the
                // second turn of a loop refused a store over a name that owns
                // nothing.
                let fresh_static = match value {
                    Val::Lit(_) => true,
                    Val::Name(m) => self.releases(*m) && st.own[*m as usize] == Own::Static,
                };
                self.take(st, value)?;
                if let Place::Name(n) = place {
                    self.wrote(st, place, self.src(*n));
                    // A borrow's binding given a fresh value (`out = out +
                    // s`) is no alias afterwards.
                    if self.borrowed(*n) {
                        st.alias[*n as usize] = None;
                        st.dead[*n as usize] = None;
                    }
                }
                match place {
                    // A store over a name that owes no release overwrites
                    // nothing: it binds the name again, and the ownership
                    // state starts over from there.
                    Place::Name(n) if self.owned(*n) && !self.releases(*n) => {
                        st.own[*n as usize] = Own::Held;
                    }
                    Place::Name(n) if self.releases(*n) => {
                        // A store over a name this path still has a value in
                        // owes the release of that value. `Static` counts:
                        // `let mut out = ""` binds a literal the emitters
                        // free like any other, and standing the release down
                        // there would leave the first `out = out + x` of
                        // every builder holding it (RFC-0125 §3 M3, the store
                        // slice). What owes nothing is `Gone`.
                        if *old == Old::Pending
                            && self.mode == Mode::Place
                            && st.own[*n as usize] != Own::Gone
                        {
                            self.owe_store(site);
                        } else if st.own[*n as usize] == Own::Held
                            && *old != Old::Released
                            && *old != Old::Transferred
                        {
                            // The first build says `Pending` here and the
                            // answer is this line: a store into a place this
                            // path still holds releases what it displaces
                            // (RFC-0125 §3 M3, the store slice). Every other
                            // word is a decision already made, and a held
                            // place under one is the leak it always was.
                            if *old == Old::Pending && self.mode == Mode::Place {
                                self.owe_store(site);
                            } else {
                                return self.refuse(format!(
                                    "{} is overwritten while still held — the old value is never released",
                                    self.info(*n)
                                ));
                            }
                        }
                        if st.own[*n as usize] == Own::Gone && *old == Old::Released {
                            return self.refuse(format!(
                                "{} is released before a store although it holds nothing",
                                self.info(*n)
                            ));
                        }
                        st.own[*n as usize] = if fresh_static { Own::Static } else { Own::Held };
                    }
                    Place::Name(_) => {}
                    other => {
                        self.store_place(st, other)?;
                        // The map keeps the key it is handed.
                        if let Place::Key(_, k) = other {
                            self.take(st, k)?;
                        }
                        if *old == Old::Unreleased {
                            return self.refuse(format!(
                                "a store into a place that owns heap releases nothing (line {})",
                                self.line_of(value)
                            ));
                        }
                        // A sub-place holds no state of its own here — the
                        // kernel tracks whole names — so the rule is over the
                        // ROOT (RFC-0125 §3 M3, the store slice). Module
                        // state owns what it holds for the whole module and
                        // nothing may consume it, a `modify` parameter is the
                        // caller's and holds what the caller gave it, and any
                        // other root owes the release exactly while this path
                        // still holds it.
                        if *old == Old::Pending {
                            // The ALIAS table's root and not the place's:
                            // RFC-0082 reads `t.xs` into a temporary before
                            // `t.xs[k] = v` stores through it, so the place
                            // this statement writes names a borrow and the
                            // ownership belongs to what the borrow reads.
                            let owes = match self.src_of(st, other).root {
                                Root::G(_) => true,
                                Root::N(n) => {
                                    matches!(
                                        self.body.names[n as usize].borrow_kind,
                                        Some(BorrowKind::Param { cap: "modify", .. })
                                    ) || (self.owned(n) && st.own[n as usize] != Own::Gone)
                                }
                            };
                            if owes {
                                self.owe_store(site);
                            }
                        }
                    }
                }
            }
            St::Drop(n, ..) => {
                let holes = self.body.names[*n as usize].holes.clone();
                self.drop(st, *n, &holes, None)?;
                self.wrote(st, &Place::Name(*n), self.src(*n));
            }
            St::Row {
                name,
                holes,
                exit,
                site,
            } => {
                self.drop(st, *name, holes, Some((*exit, *site)))?;
                self.wrote(st, &Place::Name(*name), self.src(*name));
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
                } else {
                    self.read(st, on)?;
                }
                let mut outs = Vec::new();
                for Arm {
                    binds,
                    body,
                    site,
                    index,
                    ..
                } in arms
                {
                    let mut a = st.clone();
                    for b in binds {
                        if self.owned(*b) {
                            a.own[*b as usize] = Own::Held;
                        }
                    }
                    // The binders' scope is the arm; they must be consumed
                    // within it, which `stmts` checks for what it binds and
                    // this checks for the binders.
                    if *carries {
                        self.arms.push((*site, *index, binds.clone()));
                    }
                    let walked = self.stmts(body, &mut a);
                    if *carries {
                        self.arms.pop();
                    }
                    walked?;
                    if !a.ended {
                        self.binders_end(&mut a, binds, *site, *index)?;
                    }
                    outs.push(a);
                }
                let site = arms.first().map(|a| a.site).unwrap_or(0);
                self.equalize(&mut outs, site);
                *st = self.join(&outs)?;
            }
            St::Block { site, body, .. } => {
                self.stmts_at(body, st, *site)?;
            }
            St::Loop { body, .. } => {
                self.loops.push(LoopCtx {
                    entry: st.clone(),
                    breaks: Vec::new(),
                    continues: Vec::new(),
                    bound_inside: Vec::new(),
                });
                let mut a = st.clone();
                let mark = self.refusals.len();
                self.stmts(body, &mut a)?;
                let mut ctx = self.loops.pop().unwrap();
                // A literal the body replaced: the second turn starts with
                // the value the first left, so the body is judged once more
                // from that state. Every back edge widens it, the `continue`s
                // as well as the body's end, or a `continue` after the store
                // would be judged against an entry the loop never has again.
                let mut wider = false;
                if !a.ended {
                    wider |= self.widen(&mut ctx.entry, &a);
                }
                for c in &ctx.continues.clone() {
                    wider |= self.widen(&mut ctx.entry, c);
                }
                if wider {
                    self.loops.push(LoopCtx {
                        entry: ctx.entry.clone(),
                        breaks: Vec::new(),
                        continues: Vec::new(),
                        bound_inside: Vec::new(),
                    });
                    a = ctx.entry.clone();
                    // The body is judged AGAIN, not a second time: the state
                    // it really has on the second turn is the widened one, so
                    // the first walk's refusals are what this walk replaces.
                    // Keeping both would say every mistake inside a widening
                    // loop twice (RFC-0125 §3 M3).
                    self.refusals.truncate(mark);
                    self.stmts(body, &mut a)?;
                    ctx = self.loops.pop().unwrap();
                }
                // The back edge: the fall-through end of the body, and every
                // `continue`, must find the entry state again.
                for c in &ctx.continues {
                    self.same_outside(c, &ctx.entry, &ctx.bound_inside)?;
                }
                if !a.ended {
                    self.back_edge(&mut a, &ctx)?;
                }
                *st = if ctx.breaks.is_empty() {
                    State {
                        own: st.own.clone(),
                        holes: st.holes.clone(),
                        ended: true,
                        taker: st.taker.clone(),
                        taken_at: st.taken_at.clone(),
                        dead: st.dead.clone(),
                        alias: st.alias.clone(),
                    }
                } else {
                    self.join(&ctx.breaks)?
                };
            }
            St::Break { site } => {
                let Some(l) = self.loops.last_mut() else {
                    return self.refuse("a `break` outside a loop".into());
                };
                // Names bound inside the loop go out of scope here.
                let inside = l.bound_inside.clone();
                self.scope_end(st, &inside, Exit::Break, *site)?;
                let l = self.loops.last_mut().unwrap();
                l.breaks.push(st.clone());
                st.ended = true;
            }
            St::Continue { site } => {
                let Some(ctx) = self.loops.last() else {
                    return self.refuse("a `continue` outside a loop".into());
                };
                let inside = ctx.bound_inside.clone();
                self.scope_end(st, &inside, Exit::Continue, *site)?;
                // Recorded, not judged here: the loop widens its entry from
                // every back edge before any of them is compared to it.
                let l = self.loops.last_mut().unwrap();
                l.continues.push(st.clone());
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
                }
                let exit = if *is_try { Exit::Try } else { Exit::Return };
                self.scope_end(st, &all_names(self.body), exit, *site)?;
                st.ended = true;
            }
            St::Do { rhs, .. } => self.rhs(st, rhs)?,
            St::Trap => st.ended = true,
        }
        Ok(())
    }

    /// The binders of an arm at the arm's end: refused when judging, recorded
    /// against the plan's arm table when placing.
    fn binders_end(
        &mut self,
        st: &mut State,
        binds: &[Name],
        site: usize,
        arm: u32,
    ) -> Result<(), Refusal> {
        for n in binds {
            if self.owned(*n) && st.own[*n as usize] == Own::Static {
                self.gone(st, *n);
            }
            if self.owned(*n) && !self.releases(*n) && st.own[*n as usize] == Own::Held {
                self.unbind(st, *n);
                continue;
            }
            if self.owned(*n) && st.own[*n as usize] == Own::Held {
                // The arm row carries the binder's holes (RFC-0125 M3), so a
                // binder one of whose fields the arm handed out is freed
                // minus that field.
                if self.mode == Mode::Place && site != 0 {
                    let holes = self.holes_owned(st, *n);
                    self.missing.push(Missing {
                        exit: Exit::Block,
                        site,
                        name: *n,
                        kind: MissingKind::ArmBinder { arm },
                        holes,
                    });
                    self.gone(st, *n);
                    continue;
                }
                return self.refuse(format!(
                    "{} is still held where its arm ends — no release is placed for it",
                    self.info(*n)
                ));
            }
        }
        Ok(())
    }

    /// RFC-0114 Rule N in placement mode: where one live edge of a join has
    /// taken a name another still holds, the holding edges release it, and
    /// the release is recorded against the plan's edge table. In judging
    /// mode nothing changes and `join` refuses the disagreement.
    fn equalize(&mut self, edges: &mut [State], site: usize) {
        if self.mode != Mode::Place || site == 0 {
            return;
        }
        for n in 0..self.body.names.len() as Name {
            // Rule N is about a release, so a name that owes none needs no
            // edge row: `join` reconciles its ownership state instead.
            if !self.releases(n) {
                continue;
            }
            let live: Vec<usize> = (0..edges.len()).filter(|i| !edges[*i].ended).collect();
            let held: Vec<usize> = live
                .iter()
                .copied()
                .filter(|i| edges[*i].own[n as usize] != Own::Gone)
                .collect();
            // Rule N one level down: a hole one held edge has and another
            // lacks is released as a sub-place on the edge that lacks it,
            // which then holds the same hole. An edge whose own hole
            // overlaps the path (a take above or below it) cannot release
            // the path, and is left to the judgment.
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
                    self.missing.push(Missing {
                        exit: Exit::Block,
                        site,
                        name: n,
                        kind: MissingKind::EdgePlace {
                            edge: *i as u32,
                            path: h.clone(),
                        },
                        holes: Vec::new(),
                    });
                    edges[*i].holes.push((n, h.clone()));
                    edges[*i].holes.sort();
                }
            }
            // An edge row releases the whole value, and the edge table
            // carries no holes: a name holed on any live edge gets none,
            // and is left to the judgment.
            let holed = live
                .iter()
                .any(|i| edges[*i].holes.iter().any(|(h, _)| *h == n));
            let gone = live.iter().any(|i| edges[*i].own[n as usize] == Own::Gone);
            if !gone || held.is_empty() {
                continue;
            }
            for i in held {
                if !holed {
                    self.missing.push(Missing {
                        exit: Exit::Block,
                        site,
                        name: n,
                        kind: MissingKind::Edge { edge: i as u32 },
                        holes: Vec::new(),
                    });
                }
                self.gone(&mut edges[i], n);
            }
        }
    }

    /// The holes of `n` in `st`, owned.
    fn holes_owned(&self, st: &State, n: Name) -> Vec<String> {
        self.holes_of(st, n)
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    fn line_of(&self, v: &Val) -> usize {
        match v {
            Val::Name(n) => self.body.names[*n as usize].line,
            Val::Lit(_) => 0,
        }
    }

    /// Where `at` holds a value a name was `Static` at `entry`, the entry
    /// becomes `Held`; answers whether anything changed.
    fn widen(&self, entry: &mut State, at: &State) -> bool {
        let mut changed = false;
        for n in 0..self.body.names.len() {
            if entry.own[n] == Own::Static && at.own[n] == Own::Held {
                entry.own[n] = Own::Held;
                changed = true;
            }
            // An alias ended in the body is ended when the next turn starts.
            if entry.dead[n].is_none() && at.dead[n].is_some() {
                entry.dead[n] = at.dead[n].clone();
                changed = true;
            }
        }
        changed
    }

    fn back_edge(&mut self, at: &mut State, ctx: &LoopCtx) -> Result<(), Refusal> {
        self.scope_end(at, &ctx.bound_inside, Exit::Block, 0)?;
        self.same_outside(at, &ctx.entry, &ctx.bound_inside)
    }

    /// Every owned name bound outside the loop must be as it was at entry.
    ///
    /// The entry is the join of the loop's edges after [`Self::widen`], so a
    /// back edge that owes LESS than the entry — `Static` where the entry is
    /// `Held`, a body that released the value and put a literal back — is
    /// within it and not a difference. What is a difference is a name gone on
    /// one and not the other, and a name the entry does not hold that a turn
    /// would leave held for the next one.
    fn same_outside(&self, at: &State, entry: &State, inside: &[Name]) -> Result<(), Refusal> {
        for n in 0..self.body.names.len() as Name {
            if !self.owned(n) || inside.contains(&n) {
                continue;
            }
            // A name that owes no release is judged for the one difference
            // that is about OWNERSHIP: a turn that consumed it would use it
            // again on the next one. A turn that BOUND it owes nothing to the
            // turn after, so a temporary a switch arm stores into is not a
            // difference the way a held buffer is.
            if !self.releases(n) {
                if at.own[n as usize] == Own::Gone && entry.own[n as usize] != Own::Gone {
                    let s = self.src(n);
                    return match &at.taker[n as usize] {
                        Some((l, by, _)) if !by.is_empty() => self.refuse_at(
                            *l,
                            format!(
                                "`{s}` is consumed by {by} inside a loop, so it would be used \
                                 again on the next iteration"
                            ),
                        ),
                        _ => Ok(()),
                    };
                }
                continue;
            }
            let within = at.own[n as usize] == Own::Static && entry.own[n as usize] == Own::Held;
            if at.own[n as usize] != entry.own[n as usize] && !within {
                if at.own[n as usize] == Own::Gone {
                    let s = self.src(n);
                    return match &at.taker[n as usize] {
                        Some((l, by, _)) if !by.is_empty() => self.refuse_at(
                            *l,
                            format!(
                                "`{s}` is consumed by {by} inside a loop, so it would be used \
                                 again on the next iteration"
                            ),
                        ),
                        _ => self.refuse(format!(
                            "`{s}` is released inside a loop, so it would be used again on \
                             the next iteration"
                        )),
                    };
                }
                return self.refuse(format!(
                    "{} is bound inside a loop that would use it again on the next turn",
                    self.info(n)
                ));
            }
            // A hole a turn made is a consumption the next turn would repeat,
            // and the checker says it in rule 1's loop sentence — of the PATH
            // the reader took, at the line of the take (RFC-0125 §3 M3, row
            // 25). The taker needs no field of its own: a hole is what a
            // prefix `consume` makes, and it is the only thing that makes one.
            let (before, after) = (self.holes_of(entry, n), self.holes_of(at, n));
            if before != after {
                let s = self.src(n);
                let path = after
                    .iter()
                    .find(|h| !before.contains(*h))
                    .map(|h| h.replace(".[]", "[..]"));
                return match path {
                    Some(path) => self.refuse_at(
                        self.hole_line(at, n, &path),
                        menu(
                            format!(
                                "`{s}{path}` is consumed by `consume` inside a loop, so it \
                                 would be used again on the next iteration"
                            ),
                            vec![format!("`{s}{path}.copy()` if both sides need a value")],
                        ),
                    ),
                    None => self.refuse(format!(
                        "{} has a `consume` hole at a loop's back edge it did not have at \
                         entry",
                        self.info(n)
                    )),
                };
            }
        }
        Ok(())
    }

    fn holes_of<'s>(&self, st: &'s State, n: Name) -> Vec<&'s str> {
        st.holes
            .iter()
            .filter(|(h, _)| *h == n)
            .map(|(_, p)| p.as_str())
            .collect()
    }

    /// The state after a join: every edge that reaches it agrees on every name.
    fn join(&self, edges: &[State]) -> Result<State, Refusal> {
        let live: Vec<&State> = edges.iter().filter(|s| !s.ended).collect();
        let Some(first) = live.first() else {
            return Ok(State {
                own: edges[0].own.clone(),
                holes: edges[0].holes.clone(),
                ended: true,
                taker: edges[0].taker.clone(),
                taken_at: edges[0].taken_at.clone(),
                dead: edges[0].dead.clone(),
                alias: edges[0].alias.clone(),
            });
        };
        let mut joined = (*first).clone();
        for other in &live[1..] {
            for n in 0..self.body.names.len() as Name {
                // An alias ended on one edge is ended after the join, and
                // an alias one edge bound is bound after it.
                if joined.dead[n as usize].is_none() {
                    joined.dead[n as usize] = other.dead[n as usize].clone();
                }
                if joined.alias[n as usize].is_none() {
                    joined.alias[n as usize] = other.alias[n as usize].clone();
                }
                if !self.owned(n) {
                    continue;
                }
                // A name that owes no release: the edges disagree about
                // OWNERSHIP and not about a release, so nothing has to be
                // placed and nothing is refused here. The join keeps the
                // pessimistic answer — consumed on one edge is consumed
                // after it — so a use below the join is refused instead.
                if !self.releases(n) {
                    let taken = live
                        .iter()
                        .find(|s| s.own[n as usize] == Own::Gone && s.taker[n as usize].is_some());
                    match taken {
                        Some(s) => {
                            joined.own[n as usize] = Own::Gone;
                            joined.taker[n as usize] = s.taker[n as usize].clone();
                        }
                        // Neither edge consumed it. An edge that never bound
                        // it says nothing about the edge that did.
                        None if live.iter().any(|s| s.own[n as usize] != Own::Gone) => {
                            joined.own[n as usize] = Own::Held;
                        }
                        None => {}
                    }
                    for h in self.holes_owned(other, n) {
                        if !joined.holes.iter().any(|(m, p)| *m == n && *p == h) {
                            joined.holes.push((n, h));
                        }
                    }
                    joined.holes.sort();
                    continue;
                }
                let (a, b) = (first.own[n as usize], other.own[n as usize]);
                if (a == Own::Gone) != (b == Own::Gone) {
                    let gone = if a == Own::Gone { first } else { other };
                    let s = self.src(n);
                    return match &gone.taker[n as usize] {
                        Some((l, by, _)) if !by.is_empty() => self.refuse_at(
                            *l,
                            format!(
                                "`{s}` was moved here into {by} on one path and not on the \
                                 other, and nothing releases it where the paths join"
                            ),
                        ),
                        _ => self.refuse(format!(
                            "`{s}` is released on one path and still held on another where \
                             the paths join"
                        )),
                    };
                }
                if a != b {
                    joined.own[n as usize] = Own::Held;
                }
                if a != Own::Gone && self.holes_of(first, n) != self.holes_of(other, n) {
                    return self.refuse(format!(
                        "{} has a `consume` hole on one edge of a join and not on another",
                        self.info(n)
                    ));
                }
            }
        }
        Ok(joined)
    }
}
