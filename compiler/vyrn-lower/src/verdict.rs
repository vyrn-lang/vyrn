//! Per-function verdicts over the decided core, for `vyrn why --cost`.
//!
//! [`trap_free`] judges each function the root file declares: trap-free
//! when no row its calls can reach is an explicit trap or a check row the
//! elision kept. It reads the bodies an emitter reads ([`World::body_at`]),
//! so it runs after the effect judgment, whose walk sees no check row, and
//! joins none of its sets: the bodies are its own graph, solved by
//! [`crate::fixpoint::solve`].
//!
//! The verdict does not cover what no row states: an allocation that finds
//! no memory, a call stack or a region nesting past its limit, and a failure
//! inside a builtin with an effect, which the effect set names.

use std::collections::HashMap;

use vyrn_frontend::ast::{FnId, Program};
use vyrn_frontend::core::check::Verdict;
use vyrn_frontend::core::{rows, Body, Callee, Rhs, St};
use vyrn_frontend::effects::{atom, extern_effect, Effect};

use crate::World;

/// What a row does that a trap-free function may not, or the call that
/// reaches one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trap {
    /// A call whose atom is `trap` (`panic`, `assert`), by its name, or a
    /// row that ends the path in a trap.
    Explicit(Option<String>),
    /// A check row the elision kept, by what it guards
    /// ([`crate::insight::guarded`]).
    Kept(&'static str),
    /// A call through a function value: its callee is unknown.
    Value,
    /// A call into an `extern fn` host import, which traps where the host
    /// lacks it.
    Extern(String),
    /// A call that resolves to no body to judge: a method or projection the
    /// core leaves to dispatch, or a function it did not build.
    Unbuilt(String),
    /// A call into a function that is not trap-free.
    Callee(String),
}

/// Why a function is not trap-free: a [`Trap`] and its source line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cause {
    pub trap: Trap,
    pub line: usize,
}

impl Cause {
    /// The cause as a clause after "no, ": with its line when `at` holds
    /// and the row has one.
    pub fn text(&self, at: bool) -> String {
        let (what, then) = match &self.trap {
            Trap::Explicit(Some(f)) => (format!("it calls `{f}`"), ""),
            Trap::Explicit(None) => ("it traps".into(), ""),
            Trap::Kept(what) => (format!("it keeps {} {what} check", article(what)), ""),
            Trap::Value => ("it calls a function value".into(), ""),
            Trap::Extern(f) => (format!("it calls the extern `{f}`"), ""),
            Trap::Unbuilt(f) => (format!("it calls `{f}`"), ", which has no body to judge"),
            Trap::Callee(f) => (format!("it calls `{f}`"), ", which is not trap-free"),
        };
        match at && self.line > 0 {
            true => format!("{what} at line {}{then}", self.line),
            false => format!("{what}{then}"),
        }
    }
}

fn article(word: &str) -> &'static str {
    match word.starts_with(['a', 'e', 'i', 'o', 'u']) {
        true => "an",
        false => "a",
    }
}

/// The trap-free verdict of each function the root file declares, by its
/// top frame ([`crate::insight::root_frames`]), in function-table order:
/// `None` where it is trap-free, else the first [`Cause`].
///
/// A function's own rows give its cause first, in row order. Else its cause
/// is the first call, in row order, into a callee one call nearer to an own
/// cause, so following the causes ends at a row. A recursive cycle is
/// trap-free when no frame of it reaches a cause: the least fixpoint of
/// "reaches a cause" is the complement of the greatest of "trap-free", and a
/// run that traps runs a finite chain of calls to the row that traps.
pub fn trap_free(program: &Program, world: &World) -> Vec<(FnId, Option<Cause>)> {
    let graph = Graph::of(program, world);
    let causes = graph.causes();
    let tops = crate::insight::root_frames(program, world).into_iter();
    tops.filter(|(_, b)| !b.name.contains("@lambda"))
        .filter_map(|(s, b)| Some((s, causes[*graph.at.get(&b.id?)?].clone())))
        .collect()
}

/// The frames the root file's functions reach by direct calls and declared
/// releases, numbered in the order a worklist from the root frames meets
/// them.
struct Graph {
    at: HashMap<FnId, usize>,
    /// Per frame, its first own cause.
    own: Vec<Option<Cause>>,
    /// Per frame, each call into a frame, in row order.
    calls: Vec<Vec<Call>>,
}

/// A call into a frame: the callee as the row spells it, the line, and the
/// callee's frame.
struct Call(String, usize, usize);

impl Graph {
    fn of(program: &Program, world: &World) -> Graph {
        let mut g = Graph {
            at: HashMap::new(),
            own: Vec::new(),
            calls: Vec::new(),
        };
        let mut work: Vec<&Body> = Vec::new();
        for (_, b) in crate::insight::root_frames(program, world) {
            if let Some(id) = b.id {
                g.node(id, b, &mut work);
            }
        }
        // Each frame is pushed once, when `node` numbers it, so the
        // worklist empties.
        let mut next = 0;
        while let Some(b) = work.get(next).copied() {
            let (own, calls) = Self::walk(program, world, b, &mut g, &mut work);
            g.own[next] = own;
            g.calls[next] = calls;
            next += 1;
        }
        g
    }

    /// The frame of row `id`, numbered and queued when it is new.
    fn node<'w>(&mut self, id: FnId, b: &'w Body, work: &mut Vec<&'w Body>) -> usize {
        *self.at.entry(id).or_insert_with(|| {
            work.push(b);
            self.own.push(None);
            self.calls.push(Vec::new());
            work.len() - 1
        })
    }

    /// The first own cause of `b` and its calls into frames, each callee
    /// numbered.
    fn walk<'w>(
        program: &Program,
        world: &'w World,
        b: &'w Body,
        g: &mut Graph,
        work: &mut Vec<&'w Body>,
    ) -> (Option<Cause>, Vec<Call>) {
        let sums = world.summaries();
        let mut own: Option<Cause> = None;
        let mut calls = Vec::new();
        let mut cause = |trap: Trap, line: usize| {
            own.get_or_insert(Cause { trap, line });
        };
        let mut last = 0;
        // A call into a row with a body is an edge; any other is a cause.
        let mut into = |name: &str,
                        id: Option<FnId>,
                        line: usize,
                        cause: &mut dyn FnMut(Trap, usize)| {
            match id.and_then(|id| Some((id, world.body_at(id)?))) {
                Some((id, callee)) => {
                    calls.push(Call(name.to_string(), line, g.node(id, callee, work)))
                }
                None => cause(Trap::Unbuilt(name.to_string()), line),
            }
        };
        for (s, _) in rows(&b.stmts) {
            let line = match s {
                St::Let(n, _) => b.names[n.index()].line,
                St::Do { line, .. } | St::Store { line, .. } | St::Drop(_, _, line, _) => *line,
                St::Check(c) => c.site.line,
                _ => 0,
            };
            last = if line > 0 { line } else { last };
            let rhs = match s {
                St::Trap => {
                    cause(Trap::Explicit(None), last);
                    continue;
                }
                St::Check(c) if c.verdict == Verdict::Kept => {
                    cause(Trap::Kept(crate::insight::guarded(&c.guard)), last);
                    continue;
                }
                St::Let(_, rhs) | St::Do { rhs, .. } => rhs,
                _ => continue,
            };
            let Rhs::Call { callee, kind, .. } = rhs else {
                continue;
            };
            let name = b.spelled(callee);
            if atom(callee) == Some(Effect::Trap) {
                // The loader stamps each `panic`, which no source spells so.
                let said = if vyrn_frontend::ast::is_panic(callee) {
                    "panic"
                } else {
                    name
                };
                cause(Trap::Explicit(Some(said.to_string())), last);
                continue;
            }
            match kind {
                Callee::Value(_) | Callee::Bound => cause(Trap::Value, last),
                Callee::Method | Callee::Projection => cause(Trap::Unbuilt(name.into()), last),
                Callee::Fn(f) => match program.functions.get(f.index()) {
                    Some(d) if d.is_extern => match extern_effect(d) {
                        // A host-boundary extern the runtime implements on
                        // every target.
                        Some(Effect::Clock | Effect::Random) => {}
                        _ => cause(Trap::Extern(name.into()), last),
                    },
                    _ => into(name, sums.runs(rhs), last, &mut cause),
                },
                Callee::Builtin
                | Callee::Reserved
                | Callee::Ctor
                | Callee::Named
                | Callee::Proven => {}
            }
        }
        // A name's declared releases run where it is released. A generic
        // one runs one of its instances, and none where the program makes
        // no value of its type.
        for info in b.names.iter().filter(|i| !i.borrow) {
            for r in &info.runs {
                let f = world.fn_id(r);
                let generic = f.and_then(|f| program.functions.get(f.index()));
                match generic.filter(|d| !d.type_params.is_empty()) {
                    Some(_) => (f.into_iter().flat_map(|f| sums.instances_of(f)))
                        .for_each(|i| into(b.spelled(r), Some(i), info.line, &mut cause)),
                    None => into(b.spelled(r), f, info.line, &mut cause),
                }
            }
        }
        (own, calls)
    }

    /// Each frame's first cause: its own, else the first call into a frame
    /// one call nearer to an own cause.
    fn causes(&self) -> Vec<Option<Cause>> {
        let edges: Vec<Vec<usize>> = (self.calls.iter())
            .map(|cs| cs.iter().map(|c| c.2).collect())
            .collect();
        let start = self
            .own
            .iter()
            .map(|c| Dist(c.as_ref().map(|_| 0)))
            .collect();
        let dist = crate::fixpoint::solve(start, &edges);
        (0..self.own.len())
            .map(|i| match (&self.own[i], dist[i].0) {
                (Some(c), _) => Some(c.clone()),
                (None, None) => None,
                (None, Some(d)) => {
                    // At the fixpoint a distance is one more than a callee's,
                    // the one whose join set it last.
                    let Call(name, line, _) = (self.calls[i].iter())
                        .find(|c| dist[c.2].0 == Some(d - 1))
                        .expect("a frame at distance d calls one at d - 1");
                    Some(Cause {
                        trap: Trap::Callee(name.clone()),
                        line: *line,
                    })
                }
            })
            .collect()
    }
}

/// A frame's distance in calls to its nearest own cause; `None` where it
/// reaches none. A join takes the nearer distance, so the order is the
/// reverse of the distances, and every value is the length of a call path
/// to an own cause: a distance only falls, and never below the shortest
/// path's, so the joins end.
struct Dist(Option<u32>);

impl crate::fixpoint::Lattice for Dist {
    fn join(&mut self, callee: &Dist) -> bool {
        let Some(via) = callee.0.map(|d| d + 1) else {
            return false;
        };
        if self.0.is_some_and(|d| d <= via) {
            return false;
        }
        self.0 = Some(via);
        true
    }
}
