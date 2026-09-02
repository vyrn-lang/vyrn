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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingKind {
    /// At an exit: the plan's placed-release rows.
    Exit,
    /// On one edge of a join, RFC-0114 Rule N: the plan's edge table.
    Edge { edge: u32 },
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
}

/// The state at one point: every owned name's [`Own`], plus whether the path
/// has ended.
#[derive(Clone, PartialEq, Eq, Debug)]
struct State {
    own: Vec<Own>,
    /// The path returned, broke, continued or trapped: it reaches no join.
    ended: bool,
}

/// One refusal, worded for the author of the program — and, for M2, for the
/// author of the plan.
#[derive(Debug, Clone)]
pub struct Refusal {
    pub message: String,
}

struct Kernel<'b> {
    body: &'b Body,
    mode: Mode,
    missing: Vec<Missing>,
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
    };
    let mut st = State {
        own: vec![Own::Gone; body.names.len()],
        ended: false,
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

    fn info(&self, n: Name) -> String {
        let i = &self.body.names[n as usize];
        format!("`{}` (line {})", i.source, i.line)
    }

    fn refuse<T>(&self, msg: String) -> Result<T, Refusal> {
        Err(Refusal {
            message: format!("{}: {}", self.body.name, msg),
        })
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
            if self.owned(*n) && st.own[*n as usize] == Own::Held {
                if self.mode == Mode::Place {
                    self.missing.push(Missing {
                        exit,
                        site,
                        name: *n,
                        kind: MissingKind::Exit,
                    });
                    st.own[*n as usize] = Own::Gone;
                    continue;
                }
                return self.refuse(format!(
                    "{} is still held where its scope ends — no release is placed for it",
                    self.info(*n)
                ));
            }
        }
        Ok(())
    }

    /// A read of a name: it must be held.
    fn read(&self, st: &State, v: &Val) -> Result<(), Refusal> {
        if let Val::Name(n) = v {
            if self.owned(*n) && st.own[*n as usize] != Own::Held {
                return self.refuse(format!("{} is read after it was released", self.info(*n)));
            }
        }
        Ok(())
    }

    /// A take of a name: it must be held, and it is gone afterwards.
    fn take(&self, st: &mut State, v: &Val) -> Result<(), Refusal> {
        if let Val::Name(n) = v {
            if self.owned(*n) {
                if st.own[*n as usize] != Own::Held {
                    return self.refuse(format!(
                        "{} is taken after it was already released or taken",
                        self.info(*n)
                    ));
                }
                st.own[*n as usize] = Own::Gone;
            }
        }
        Ok(())
    }

    fn place(&self, st: &State, p: &Place) -> Result<(), Refusal> {
        match p {
            Place::Name(n) => self.read(st, &Val::Name(*n)),
            Place::Global(_) => Ok(()),
            Place::Field(b, _) => self.place(st, b),
            Place::Elem(b, i) => {
                self.place(st, b)?;
                self.read(st, i)
            }
        }
    }

    fn rhs(&self, st: &mut State, r: &Rhs) -> Result<(), Refusal> {
        match r {
            Rhs::Val(v) => self.take(st, v),
            Rhs::Read(p) => self.place(st, p),
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

    fn stmt(&mut self, s: &St, st: &mut State, bound_here: &mut Vec<Name>) -> Result<(), Refusal> {
        match s {
            St::Let(n, rhs) => {
                self.rhs(st, rhs)?;
                if self.owned(*n) {
                    st.own[*n as usize] = Own::Held;
                    bound_here.push(*n);
                    if let Some(l) = self.loops.last_mut() {
                        l.bound_inside.push(*n);
                    }
                }
            }
            St::Store { place, value, old } => {
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
                        st.own[*n as usize] = Own::Held;
                    }
                    Place::Name(_) => {}
                    other => {
                        self.place(st, other)?;
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
                if !self.owned(*n) {
                    return self.refuse(format!(
                        "{} is released although the body does not own it",
                        self.info(*n)
                    ));
                }
                if st.own[*n as usize] != Own::Held {
                    return self.refuse(format!(
                        "{} is released twice, or released after it was taken",
                        self.info(*n)
                    ));
                }
                st.own[*n as usize] = Own::Gone;
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
                let ctx = self.loops.pop().unwrap();
                // The back edge: the fall-through end of the body, and every
                // `continue`, must find the entry state again.
                if !a.ended {
                    self.back_edge(&mut a, &ctx)?;
                }
                *st = if ctx.breaks.is_empty() {
                    State {
                        own: st.own.clone(),
                        ended: true,
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
            } => {
                if let Some(v) = value {
                    self.take(st, v)?;
                }
                let exit = if *is_try { Exit::Try } else { Exit::Return };
                self.scope_end(st, &all_names(self.body), exit, *site)?;
                st.ended = true;
            }
            St::Do(rhs) => self.rhs(st, rhs)?,
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
            if self.owned(*n) && st.own[*n as usize] == Own::Held {
                if self.mode == Mode::Place && site != 0 {
                    self.missing.push(Missing {
                        exit: Exit::Block,
                        site,
                        name: *n,
                        kind: MissingKind::ArmBinder { arm },
                    });
                    st.own[*n as usize] = Own::Gone;
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
            let gone = live.iter().any(|i| edges[*i].own[n as usize] == Own::Gone);
            let held: Vec<usize> = live
                .iter()
                .copied()
                .filter(|i| edges[*i].own[n as usize] == Own::Held)
                .collect();
            if !gone || held.is_empty() {
                continue;
            }
            for i in held {
                self.missing.push(Missing {
                    exit: Exit::Block,
                    site,
                    name: n,
                    kind: MissingKind::Edge { edge: i as u32 },
                });
                edges[i].own[n as usize] = Own::Gone;
            }
        }
    }

    fn line_of(&self, v: &Val) -> usize {
        match v {
            Val::Name(n) => self.body.names[*n as usize].line,
            Val::Lit => 0,
        }
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
                return self.refuse(format!(
                    "{} is {} inside a loop that would use it again on the next turn",
                    self.info(n),
                    if at.own[n as usize] == Own::Gone {
                        "taken or released"
                    } else {
                        "bound"
                    }
                ));
            }
        }
        Ok(())
    }

    /// The state after a join: every edge that reaches it agrees on every name.
    fn join(&self, edges: &[State]) -> Result<State, Refusal> {
        let live: Vec<&State> = edges.iter().filter(|s| !s.ended).collect();
        let Some(first) = live.first() else {
            return Ok(State {
                own: edges[0].own.clone(),
                ended: true,
            });
        };
        for other in &live[1..] {
            for n in 0..self.body.names.len() as Name {
                if !self.owned(n) {
                    continue;
                }
                if first.own[n as usize] != other.own[n as usize] {
                    return self.refuse(format!(
                        "{} is released on one edge of a join and still held on another",
                        self.info(n)
                    ));
                }
            }
        }
        Ok((*first).clone())
    }
}
