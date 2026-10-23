//! Per-function verdicts over the decided core, for `vyrn why --cost`.
//!
//! [`table`] judges each function the root file declares twice. It is
//! trap-free when no row its calls can reach is an explicit trap or a check
//! row the elision kept ([`Cause`]). It is a kernel, a function a GPU could
//! run, when it passes every rule of [`Unfit`]. Both read the bodies an
//! emitter reads ([`World::body_at`]), so they run after the effect
//! judgment, whose walk sees no check row, and join none of its sets: the
//! bodies are their own graph, solved by [`crate::fixpoint`].
//!
//! The trap-free verdict does not cover what no row states: an allocation
//! that finds no memory, a call stack or a region nesting past its limit,
//! and a failure inside a builtin with an effect, which the effect set
//! names.

use std::collections::HashMap;

use vyrn_frontend::ast::{FnId, Program, Type};
use vyrn_frontend::core::check::Verdict;
use vyrn_frontend::core::{rows, Body, BorrowKind, Callee, Rhs, St};
use vyrn_frontend::effects::{atom, extern_effect, Effect, Effects};

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

/// Why a function is not a kernel: the first rule it fails, in this order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unfit {
    /// Its effect set is not empty; `None` where the judgment has no row
    /// for it.
    Effects(Option<Effects>),
    /// It is not trap-free.
    Traps,
    /// It is in a cycle of the call graph.
    Recursive,
    /// A parameter, local or result is not `Int32`, `UInt32` or `Bool`, or a
    /// `read` array of one: who, its type as spelled, and the type's kind.
    Type(String, String, Scalar),
    /// A loop that is no counter loop ([`crate::elide::counter_loop`]), at
    /// its first line.
    Loop(usize),
    /// A call into a function that is not a kernel, by name and line.
    Callee(String, usize),
}

/// A type as a kernel sees it. WGSL has no 64-bit integer or float, and it
/// leaves `f32` rounding to the device, so only 32-bit integers and `Bool`
/// fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scalar {
    Fits,
    Wide,
    Float32,
    Float64,
    Other,
}

impl Unfit {
    /// The rule as a clause after "no, ": with its line when `at` holds and
    /// the row has one.
    pub fn text(&self, at: bool) -> String {
        let line = |l: &usize| match at && *l > 0 {
            true => format!(" at line {l}"),
            false => String::new(),
        };
        match self {
            Unfit::Effects(Some(e)) => format!("its effect set is {e}"),
            Unfit::Effects(None) => "its effect set is unknown".into(),
            Unfit::Traps => "it is not trap-free".into(),
            Unfit::Recursive => "it is recursive".into(),
            Unfit::Type(who, ty, kind) => format!(
                "{who} is {ty}, {}",
                match kind {
                    Scalar::Wide => "and WGSL has no 64-bit integer",
                    Scalar::Float64 => "and WGSL has no 64-bit float",
                    Scalar::Float32 => "and a kernel computes only integers and Bool",
                    Scalar::Fits | Scalar::Other =>
                        "not Int32, UInt32, Bool or a read array of one",
                }
            ),
            Unfit::Loop(l) => format!("the loop{} is no counter loop", line(l)),
            Unfit::Callee(f, l) => format!("it calls `{f}`{}, which is not a kernel", line(l)),
        }
    }
}

/// One function's verdicts: `None` where it is trap-free and where it is a
/// kernel.
#[derive(Debug, Clone)]
pub struct Verdicts {
    pub id: FnId,
    pub trap: Option<Cause>,
    pub kernel: Option<Unfit>,
}

/// The verdicts of each function the root file declares, by its top frame
/// ([`crate::insight::root_frames`]), in function-table order. `wide`
/// counts a 64-bit integer as 32-bit, which says what a rewrite to `Int32`
/// would make a kernel.
///
/// A function's own rows give its [`Cause`] first, in row order. Else its
/// cause is the first call, in row order, into a callee one call nearer to
/// an own cause, so following the causes ends at a row. A recursive cycle is
/// trap-free when no frame of it reaches a cause: the least fixpoint of
/// "reaches a cause" is the complement of the greatest of "trap-free", and a
/// run that traps runs a finite chain of calls to the row that traps.
pub fn table(program: &Program, world: &World, wide: bool) -> Vec<Verdicts> {
    let frames = crate::insight::root_frames(program, world).into_iter();
    let tops: Vec<(FnId, &Body)> = frames
        .filter(|(_, b)| !b.name.contains("@lambda"))
        .collect();
    let graph = Graph::of(program, world, &tops);
    let causes = graph.causes();
    let unfit = graph.kernels(program, world, &causes, wide);
    (tops.into_iter())
        .filter_map(|(id, b)| {
            let i = *graph.at.get(&b.id?)?;
            let (trap, kernel) = (causes[i].clone(), unfit[i].clone());
            Some(Verdicts { id, trap, kernel })
        })
        .collect()
}

/// The frames the root file's functions reach by direct calls and declared
/// releases, numbered in the order a walk from the root frames meets them.
struct Graph<'w> {
    at: HashMap<FnId, usize>,
    /// Per frame, its row and body.
    frames: Vec<(FnId, &'w Body)>,
    /// Per frame, its first own cause.
    own: Vec<Option<Cause>>,
    /// Per frame, each call into a frame, in row order.
    calls: Vec<Vec<Call>>,
}

/// A call into a frame: the callee as the row spells it, the line, and the
/// callee's frame.
struct Call(String, usize, usize);

impl<'w> Graph<'w> {
    fn of(program: &Program, world: &'w World, tops: &[(FnId, &'w Body)]) -> Graph<'w> {
        let mut g = Graph {
            at: HashMap::new(),
            frames: Vec::new(),
            own: Vec::new(),
            calls: Vec::new(),
        };
        for (_, b) in tops {
            if let Some(id) = b.id {
                g.node(id, b);
            }
        }
        // Each frame is pushed once, when `node` numbers it, so the walk
        // ends.
        let mut next = 0;
        while let Some(&(_, b)) = g.frames.get(next) {
            (g.own[next], g.calls[next]) = g.walk(program, world, b);
            next += 1;
        }
        g
    }

    /// The frame of row `id`, numbered the first time it is met.
    fn node(&mut self, id: FnId, b: &'w Body) -> usize {
        *self.at.entry(id).or_insert_with(|| {
            self.frames.push((id, b));
            self.own.push(None);
            self.calls.push(Vec::new());
            self.frames.len() - 1
        })
    }

    /// The first own cause of `b` and its calls into frames, each callee
    /// numbered.
    fn walk(
        &mut self,
        program: &Program,
        world: &'w World,
        b: &'w Body,
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
                    calls.push(Call(name.to_string(), line, self.node(id, callee)))
                }
                None => cause(Trap::Unbuilt(name.to_string()), line),
            }
        };
        for (s, _) in rows(&b.stmts) {
            last = line_of(s, b).unwrap_or(last);
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
            let Rhs::Call { callee, .. } = rhs else {
                continue;
            };
            let name = b.spelled(callee);
            match reach(program, world, b, rhs) {
                Reach::Free => {}
                Reach::Traps(t) => cause(t, last),
                Reach::Runs(id) => into(name, id, last, &mut cause),
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

    /// Per frame, the frames it calls.
    fn edges(&self) -> Vec<Vec<usize>> {
        (self.calls.iter())
            .map(|cs| cs.iter().map(|c| c.2).collect())
            .collect()
    }

    /// Each frame's first cause: its own, else the first call into a frame
    /// one call nearer to an own cause.
    fn causes(&self) -> Vec<Option<Cause>> {
        let edges = self.edges();
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

/// What a call row does about traps.
enum Reach {
    /// It cannot trap: a builtin without the `trap` atom, a constructor, or
    /// an extern the runtime implements on every target.
    Free,
    /// It may trap by itself.
    Traps(Trap),
    /// It runs the body of this row, or of no row the core built.
    Runs(Option<FnId>),
}

/// What the call `rhs` of `b` does about traps. `rhs` is a [`Rhs::Call`].
fn reach(program: &Program, world: &World, b: &Body, rhs: &Rhs) -> Reach {
    let Rhs::Call { callee, kind, .. } = rhs else {
        return Reach::Free;
    };
    let name = b.spelled(callee);
    if atom(callee) == Some(Effect::Trap) {
        // The loader stamps each `panic`, which no source spells so.
        let said = if vyrn_frontend::ast::is_panic(callee) {
            "panic"
        } else {
            name
        };
        return Reach::Traps(Trap::Explicit(Some(said.to_string())));
    }
    match kind {
        Callee::Value(_) | Callee::Bound => Reach::Traps(Trap::Value),
        Callee::Method | Callee::Projection => Reach::Traps(Trap::Unbuilt(name.into())),
        Callee::Fn(f) => match program.functions.get(f.index()) {
            Some(d) if d.is_extern => match extern_effect(d) {
                // A host-boundary extern the runtime implements on every
                // target.
                Some(Effect::Clock | Effect::Random) => Reach::Free,
                _ => Reach::Traps(Trap::Extern(name.into())),
            },
            _ => Reach::Runs(world.summaries().runs(rhs)),
        },
        Callee::Builtin | Callee::Reserved | Callee::Ctor | Callee::Named | Callee::Proven => {
            Reach::Free
        }
    }
}

/// The trap-free verdict of every frame the root file's frames, lambdas
/// included, reach by calls, which [`TrapFree::traps`] reads per call row.
pub struct TrapFree<'w> {
    program: &'w Program,
    world: &'w World,
    free: HashMap<FnId, bool>,
}

impl<'w> TrapFree<'w> {
    pub fn of(program: &'w Program, world: &'w World) -> TrapFree<'w> {
        let tops = crate::insight::root_frames(program, world);
        let graph = Graph::of(program, world, &tops);
        let causes = graph.causes();
        let free = (graph.at.iter())
            .map(|(id, i)| (*id, causes[*i].is_none()))
            .collect();
        TrapFree {
            program,
            world,
            free,
        }
    }

    /// Whether the call `rhs`, a row of a frame [`TrapFree::of`] walked, may
    /// trap. A call into a frame the walk did not reach may.
    pub fn traps(&self, b: &Body, rhs: &Rhs) -> bool {
        match reach(self.program, self.world, b, rhs) {
            Reach::Free => false,
            Reach::Traps(_) | Reach::Runs(None) => true,
            Reach::Runs(Some(id)) => !self.free.get(&id).copied().unwrap_or(false),
        }
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

impl Graph<'_> {
    /// Each frame's first rule it fails as a kernel. Frames go by component,
    /// callees first, so each callee is judged before its callers; a frame
    /// of a cycle fails by its own rules or by the cycle.
    fn kernels(
        &self,
        program: &Program,
        world: &World,
        causes: &[Option<Cause>],
        wide: bool,
    ) -> Vec<Option<Unfit>> {
        let edges = self.edges();
        let mut out: Vec<Option<Unfit>> = vec![None; self.frames.len()];
        for comp in crate::fixpoint::components(&edges) {
            let cyclic = comp.len() > 1 || edges[comp[0]].contains(&comp[0]);
            for &i in &comp {
                let (id, b) = self.frames[i];
                let callee = (self.calls[i].iter()).find(|c| out[c.2].is_some());
                out[i] = match world.state_use(id).map(|u| u.effects) {
                    Some(e) if !e.is_pure() => Some(Unfit::Effects(Some(e))),
                    None => Some(Unfit::Effects(None)),
                    Some(_) if causes[i].is_some() => Some(Unfit::Traps),
                    Some(_) if cyclic => Some(Unfit::Recursive),
                    Some(_) => (unfit_type(program, world, id, b, wide))
                        .or_else(|| unfit_loop(b))
                        .or_else(|| callee.map(|c| Unfit::Callee(c.0.clone(), c.1))),
                };
            }
        }
        out
    }
}

/// The source line of the row `s` of `b`; `None` for a row without one.
fn line_of(s: &St, b: &Body) -> Option<usize> {
    let line = match s {
        St::Let(n, _) => b.names[n.index()].line,
        St::Do { line, .. } | St::Store { line, .. } | St::Drop(_, _, line, _) => *line,
        St::Check(c) => c.site.line,
        _ => 0,
    };
    (line > 0).then_some(line)
}

/// The first parameter, local or result of the frame `b` of row `id` whose
/// type no kernel holds: the names in order, then the result.
fn unfit_type(program: &Program, world: &World, id: FnId, b: &Body, wide: bool) -> Option<Unfit> {
    let decls = world.ownership.proto.types();
    let resolve = |t: &Type| vyrn_frontend::types::resolve(t, decls);
    let scalar = |t: &Type| match resolve(t) {
        Type::Bool | Type::IntN { bits: 32, .. } => Scalar::Fits,
        Type::Int | Type::IntN { bits: 64, .. } if wide => Scalar::Fits,
        Type::Int | Type::IntN { bits: 64, .. } => Scalar::Wide,
        Type::Float32 => Scalar::Float32,
        Type::Float => Scalar::Float64,
        _ => Scalar::Other,
    };
    for (i, info) in b.names.iter().enumerate() {
        let ty = resolve(&info.ty);
        let read =
            !matches!(&info.borrow_kind, Some(BorrowKind::Param { cap, .. }) if *cap != "read");
        let kind = match ty.elem().filter(|_| ty.is_seq()) {
            Some(e) => match scalar(e) {
                Scalar::Fits if !(info.borrow && read) => Scalar::Other,
                k => k,
            },
            None => scalar(&ty),
        };
        if kind != Scalar::Fits {
            let n = vyrn_frontend::core::Name(i as u32);
            let who = match info.source.starts_with('@') {
                true => b.spoken(n).into_owned(),
                false => format!("`{}`", b.spoken(n)),
            };
            return Some(Unfit::Type(who, b.speech().ty(&info.ty).to_string(), kind));
        }
    }
    let row = world.fn_rows().get(id.index());
    let (f, args) = match row.and_then(|r| r.generic.as_ref()) {
        Some((g, args)) => (*g, args.iter().cloned().collect()),
        None => (id, HashMap::new()),
    };
    let ret =
        (program.functions.get(f.index())).map(|f| vyrn_frontend::types::substitute(&f.ret, &args));
    let said = |t: &Type| b.speech().ty(t).to_string();
    match ret {
        Some(t) if scalar(&t) == Scalar::Fits => None,
        Some(t) => Some(Unfit::Type("the result".into(), said(&t), scalar(&t))),
        None => Some(Unfit::Type(
            "the result".into(),
            "unknown".into(),
            Scalar::Other,
        )),
    }
}

/// The first loop of `b` that is no counter loop, at its first line.
fn unfit_loop(b: &Body) -> Option<Unfit> {
    rows(&b.stmts).find_map(|(s, _)| match s {
        St::Loop { body, .. } if !crate::elide::counter_loop(body, &b.names) => {
            let line = rows(body).find_map(|(s, _)| line_of(s, b));
            Some(Unfit::Loop(line.unwrap_or(0)))
        }
        _ => None,
    })
}
