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
    taker: Vec<Option<(usize, String)>>,
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
fn root_of(p: &Place) -> Option<(Name, String)> {
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
    /// The line of the statement being judged, and what it takes with, in
    /// the checker's words — recorded against every name it consumes.
    here: usize,
    by: String,
    /// The loop being walked: the state at its entry, the states at its
    /// `break`s, and the names bound inside it (which its back edge must find
    /// consumed).
    loops: Vec<LoopCtx>,
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
    run(body, Mode::Judge).map(|_| ())
}

/// The releases the plan owes this body and did not place. `Err` when the
/// body is refused for another reason — a double free, a use after release —
/// which no placement repairs.
pub fn placement(body: &Body) -> Result<Vec<Missing>, Refusal> {
    run(body, Mode::Place)
}

fn run(body: &Body, mode: Mode) -> Result<Vec<Missing>, Refusal> {
    let mut k = Kernel {
        body,
        mode,
        missing: Vec::new(),
        loops: Vec::new(),
        here: 0,
        by: String::new(),
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
    k.stmts(&body.stmts, &mut st)?;
    if !st.ended {
        // The parameters: the plan releases them at the body's own block.
        let site = match body.stmts.first() {
            Some(St::Block { site, .. }) => *site,
            _ => 0,
        };
        k.scope_end(&mut st, &all_names(body), Exit::Block, site)?;
    }
    Ok(k.missing)
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
        st.taker[n as usize] = Some((self.here, self.by.clone()));
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

    fn src(&self, n: Name) -> &str {
        &self.body.names[n as usize].source
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
    fn alias_take(&self, st: &State, n: Name) -> Refusal {
        let (s, src, by) = (self.src(n), self.src_text(st, n), &self.by);
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
        }
        let what = format!("it is read out of `{src}`, a place that owns it");
        // A named binding a call takes: at the binding, as the checker words
        // it, so the `.copy()` on the menu lands where the read is.
        if by.ends_with("(..)`") && !s.starts_with('@') {
            let (here, at) = (self.here, self.body.names[n as usize].line);
            return self
                .refuse_at::<()>(
                    at,
                    format!(
                        "`{s}` is read out of `{src}` here — a place that owns it\nline {here}: \
                         ... and {by} takes `{s}`, so `{s}` must be a value of its own"
                    ),
                )
                .unwrap_err();
        }
        // A `drop` is the one exit whose sentence names no place, because its
        // ways out are both about the BINDING: take the value there, or let
        // the place that owns it release it (RFC-0089 rule 4, RFC-0125 §3 M3,
        // row 21).
        if by == "a `drop`" {
            return self
                .refuse_at::<()>(
                    self.here,
                    menu(
                        format!(
                            "`{s}` may not be dropped — it is read out of a place that owns it"
                        ),
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
        let msg = if by.ends_with("(..)`") {
            format!("`{s}` may not be passed to a `consume` parameter via {by} — {what}")
        } else if by == "a `return`" {
            format!("`{s}` may not be returned — {what}")
        } else if by == "a literal" {
            format!("`{s}` may not be stored into the literal — {what}")
        } else {
            format!("`{s}` may not be stored into {by} — {what}")
        };
        self.refuse_at::<()>(self.here, msg).unwrap_err()
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
            Some((l, by)) if by.ends_with("(..)`") || by == "`drop`" => self.refuse_at::<()>(
                here,
                format!(
                    "`{read}` is {what} here but was already consumed by {by} on line {l}{note}"
                ),
            ),
            Some((l, by)) if !by.is_empty() => self.refuse_at::<()>(
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
                    self.missing.push(Missing {
                        exit,
                        site,
                        name: *n,
                        kind: MissingKind::Exit,
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
            if st.alias[n as usize].is_some() {
                let by = std::mem::replace(&mut self.by, "a `drop`".to_string());
                let r = self.alias_take(st, n);
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
        } else if by.ends_with("(..)`") {
            format!("`{s}` may not be passed to a `consume` parameter via {by} — it is {what}")
        } else if by == "a literal" {
            format!("`{s}` may not be stored into the literal — it is {what}")
        } else {
            format!("`{s}` may not be stored into {by} — it is {what}")
        };
        // The ways out, as `movecheck::Borrow::fixes` and
        // `movecheck::MoveCheck::fixes_here` name them. An `export extern fn`
        // has one: its JS caller releases the String the call returns, so the
        // signature refuses `consume` and only a copy is left (RFC-0089 M3b).
        let capture = matches!(b, BorrowKind::Capture);
        let fixes = if by == "a `return`" && capture {
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
                if let Some(b) = &self.body.names[*n as usize].borrow_kind {
                    if self.borrowed(*n) {
                        return Err(self.param_take(*n, b));
                    }
                }
            }
            if st.alias[*n as usize].is_some() {
                self.alias_read(st, *n, "used")?;
                if self.moves(*n, consume) {
                    return Err(self.alias_take(st, *n));
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
                return self.refuse_at(
                    l,
                    format!(
                        "`{s}{h}` was moved here into `consume`\nline {here}: ... and `{s}{path}` \
                         is used again here"
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
                declared,
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
                        self.take_arg(st, v, *write_back && i == 0, *declared)?;
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
            Rhs::Prim(vs, _) => {
                for v in vs {
                    self.read(st, v)?;
                }
                Ok(())
            }
            Rhs::Make(vs) => {
                for v in vs {
                    self.take(st, v)?;
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

    fn stmts_at(&mut self, stmts: &[St], st: &mut State, site: usize) -> Result<(), Refusal> {
        let mut bound_here: Vec<Name> = Vec::new();
        for s in stmts {
            if st.ended {
                // Code after a return, break or continue: the checker has
                // already refused what it can; nothing here runs.
                break;
            }
            self.stmt(s, st, &mut bound_here)?;
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
                _ => "a value".to_string(),
            },
            // RFC-0125 §3 M3, the containment slice: a spawned call is named
            // `spawn f(..)`, as `movecheck` names it. The taker is the task,
            // and a reader who wrote `spawn` is told so.
            Rhs::Call { callee, spawn, .. } => format!(
                "`{}{}(..)`",
                if *spawn { "spawn " } else { "" },
                callee.trim_start_matches('@')
            ),
            Rhs::Take(_) => "`consume`".to_string(),
            Rhs::Make(_) => "a literal".to_string(),
            Rhs::Read(_) | Rhs::Prim(..) => String::new(),
        }
    }

    fn stmt(&mut self, s: &St, st: &mut State, bound_here: &mut Vec<Name>) -> Result<(), Refusal> {
        // The line and the taker every consumption in this statement is
        // recorded with (RFC-0125 M3, third slice).
        match s {
            St::Let(n, rhs) => {
                self.here = self.body.names[*n as usize].line;
                self.by = self.by_of(rhs, Some(*n));
            }
            St::Store { place, line, .. } => {
                self.here = *line;
                self.by = match place {
                    Place::Name(n) if !self.src(*n).starts_with('@') => {
                        format!("the binding `{}`", self.src(*n))
                    }
                    Place::Field(_, f) => format!("the field `{f}`"),
                    _ => "a store".to_string(),
                };
            }
            St::Return { line, .. } => {
                self.here = *line;
                self.by = "a `return`".to_string();
            }
            St::Do(rhs, line) => {
                self.here = *line;
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
            St::Drop(n, ..) | St::Row { name: n, .. } => {
                self.here = self.body.names[*n as usize].line;
                self.by = String::new();
            }
            _ => {}
        }
        match s {
            St::Let(n, rhs) => {
                // A literal, or a literal built from literals (`[]`,
                // `Body { nodes: [] }`), owns no heap yet.
                // `Static` is a statement about a RELEASE — there is none
                // until a store gives the name a buffer — so a name that
                // owes none is never `Static`, only held or gone.
                let is_static = self.releases(*n)
                    && match rhs {
                        Rhs::Val(Val::Lit) => true,
                        Rhs::Make(vs) => vs.iter().all(|v| match v {
                            Val::Lit => true,
                            Val::Name(m) => {
                                !self.releases(*m) || st.own[*m as usize] == Own::Static
                            }
                        }),
                        _ => false,
                    };
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
                place, value, old, ..
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
                    Val::Lit => true,
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
                        if st.own[*n as usize] == Own::Held
                            && *old != Old::Released
                            && *old != Old::Transferred
                        {
                            return self.refuse(format!(
                                "{} is overwritten while still held — the old value is never released",
                                self.info(*n)
                            ));
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
                    self.stmts(body, &mut a)?;
                    if !a.ended {
                        self.binders_end(&mut a, binds, *site, *index)?;
                    }
                    outs.push(a);
                }
                let site = arms.first().map(|a| a.site).unwrap_or(0);
                self.equalize(&mut outs, site);
                *st = self.join(&outs)?;
            }
            St::Block { site, body } => {
                self.stmts_at(body, st, *site)?;
            }
            St::Loop(body) => {
                self.loops.push(LoopCtx {
                    entry: st.clone(),
                    breaks: Vec::new(),
                    continues: Vec::new(),
                    bound_inside: Vec::new(),
                });
                let mut a = st.clone();
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
            St::Do(rhs, _) => self.rhs(st, rhs)?,
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
            Val::Lit => 0,
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
                        Some((l, by)) if !by.is_empty() => self.refuse_at(
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
                        Some((l, by)) if !by.is_empty() => self.refuse_at(
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
            if self.holes_of(at, n) != self.holes_of(entry, n) {
                return self.refuse(format!(
                    "{} has a `consume` hole at a loop's back edge it did not have at entry",
                    self.info(n)
                ));
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
                        Some((l, by)) if !by.is_empty() => self.refuse_at(
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
