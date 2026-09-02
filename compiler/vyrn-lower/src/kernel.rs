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
//! Names the body does not own — borrowed parameters, pattern binders of a
//! non-consuming switch, heapless values — are invisible here.

use crate::core::{Arm, Body, Name, Old, Place, Rhs, St, Val};
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
    };
    for p in &body.params {
        if body.names[*p as usize].owned {
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
    fn owned(&self, n: Name) -> bool {
        self.body.names[n as usize].owned
    }

    /// The name is consumed: nothing to release, and no holes to remember.
    /// What consumed it, and where, is kept for the wording.
    fn gone(&self, st: &mut State, n: Name) {
        st.own[n as usize] = Own::Gone;
        st.holes.retain(|(h, _)| *h != n);
        st.taker[n as usize] = Some((self.here, self.by.clone()));
    }

    fn src(&self, n: Name) -> &str {
        &self.body.names[n as usize].source
    }

    fn info(&self, n: Name) -> String {
        let i = &self.body.names[n as usize];
        format!("`{}` (line {})", i.source, i.line)
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
        let s = self.src(n);
        let here = self.here;
        let r = match &st.taker[n as usize] {
            Some((l, by)) if by.ends_with("(..)`") => self.refuse_at::<()>(
                here,
                format!(
                    "`{s}` is {what} here but was already consumed by {by} on line {l}\n  \
                     (a `consume` parameter takes ownership; the value can't be used afterward)"
                ),
            ),
            Some((l, by)) if !by.is_empty() => self.refuse_at::<()>(
                *l,
                format!("`{s}` was moved here into {by}\nline {here}: ... and `{s}` is {what} again here"),
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
            return self.refuse(format!(
                "{} is released although the body does not own it",
                self.info(n)
            ));
        }
        if st.own[n as usize] == Own::Gone {
            return Err(self.used_after(st, n, "released"));
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

    /// A read of a name: it must be held.
    fn read(&self, st: &State, v: &Val) -> Result<(), Refusal> {
        if let Val::Name(n) = v {
            if self.owned(*n) && st.own[*n as usize] == Own::Gone {
                return Err(self.used_after(st, *n, "used"));
            }
        }
        Ok(())
    }

    /// A take of a name: it must be held, and it is gone afterwards.
    fn take(&self, st: &mut State, v: &Val) -> Result<(), Refusal> {
        if let Val::Name(n) = v {
            if self.owned(*n) {
                if st.own[*n as usize] == Own::Gone {
                    return Err(self.used_after(st, *n, "used"));
                }
                if let Some((_, path)) = st.holes.iter().find(|(h, _)| h == n) {
                    let s = self.src(*n);
                    let (here, l) = (self.here, self.hole_line(st, *n, path));
                    return self.refuse_at(
                        l,
                        format!(
                            "`{s}{path}` was taken out of `{s}` here\nline {here}: ... and `{s}` \
                             is used as a whole here, with the hole still in it"
                        ),
                    );
                }
                self.gone(st, *n);
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
        self.read(st, &Val::Name(n))?;
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
            Rhs::Call { args, .. } => {
                // Reads first, takes after: the call sees every argument
                // before it owns any, so a receiver handed back through the
                // result (`dup.append(dup)`) is read and taken by one call.
                for (v, cap) in args {
                    if !matches!(cap, Capability::Consume) {
                        self.read(st, v)?;
                    }
                }
                for (v, cap) in args {
                    if matches!(cap, Capability::Consume) {
                        self.take(st, v)?;
                    }
                }
                Ok(())
            }
            Rhs::Prim(vs) => {
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
            Rhs::Call { callee, .. } => format!("`{}(..)`", callee.trim_start_matches('@')),
            Rhs::Take(_) => "`consume`".to_string(),
            Rhs::Make(_) => "a literal".to_string(),
            Rhs::Read(_) | Rhs::Prim(_) => String::new(),
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
            St::Drop(n) | St::Row { name: n, .. } => {
                self.here = self.body.names[*n as usize].line;
                self.by = String::new();
            }
            _ => {}
        }
        match s {
            St::Let(n, rhs) => {
                // A literal, or a literal built from literals (`[]`,
                // `Body { nodes: [] }`), owns no heap yet.
                let is_static = match rhs {
                    Rhs::Val(Val::Lit) => true,
                    Rhs::Make(vs) => vs.iter().all(|v| match v {
                        Val::Lit => true,
                        Val::Name(m) => !self.owned(*m) || st.own[*m as usize] == Own::Static,
                    }),
                    _ => false,
                };
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
                self.take(st, value)?;
                match place {
                    Place::Name(n) if self.owned(*n) => {
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
                        st.own[*n as usize] = if matches!(value, Val::Lit) {
                            Own::Static
                        } else {
                            Own::Held
                        };
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
            St::Drop(n) => {
                let holes = self.body.names[*n as usize].holes.clone();
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
                    bound_inside: Vec::new(),
                });
                let mut a = st.clone();
                self.stmts(body, &mut a)?;
                let mut ctx = self.loops.pop().unwrap();
                // A literal the body replaced: the second turn starts with
                // the value the first left, so the body is judged once more
                // from that state.
                if !a.ended && self.widen(&mut ctx.entry, &a) {
                    self.loops.push(LoopCtx {
                        entry: ctx.entry.clone(),
                        breaks: Vec::new(),
                        bound_inside: Vec::new(),
                    });
                    a = ctx.entry.clone();
                    self.stmts(body, &mut a)?;
                    ctx = self.loops.pop().unwrap();
                }
                // The back edge: the fall-through end of the body, and every
                // `continue`, must find the entry state again.
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
                let entry = ctx.entry.clone();
                let inside = ctx.bound_inside.clone();
                self.scope_end(st, &inside, Exit::Continue, *site)?;
                self.same_outside(st, &entry, &inside)?;
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
            if !self.owned(n) {
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
        }
        changed
    }

    fn back_edge(&mut self, at: &mut State, ctx: &LoopCtx) -> Result<(), Refusal> {
        self.scope_end(at, &ctx.bound_inside, Exit::Block, 0)?;
        self.same_outside(at, &ctx.entry, &ctx.bound_inside)
    }

    /// Every owned name bound outside the loop must be as it was at entry.
    fn same_outside(&self, at: &State, entry: &State, inside: &[Name]) -> Result<(), Refusal> {
        for n in 0..self.body.names.len() as Name {
            if !self.owned(n) || inside.contains(&n) {
                continue;
            }
            if at.own[n as usize] != entry.own[n as usize] {
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
            });
        };
        let mut joined = (*first).clone();
        for other in &live[1..] {
            for n in 0..self.body.names.len() as Name {
                if !self.owned(n) {
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
