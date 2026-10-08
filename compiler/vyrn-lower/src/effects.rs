//! The effect judgment: a body's effect set is the join
//! of its own atoms and its callees' sets, taken to a fixpoint so recursion
//! ends. [`atoms`] is the second column of the effect table;
//! `tests/effects.rs` refuses to run if the two differ. The judgment sees a
//! call by callee name, an owned name born of a primitive or literal (an
//! allocation), a global place (module state), and a trap. For a call through
//! a function value the caller answers with the closed set of functions its
//! type may hold (`StoredFnEffects::every_source`). A lambda is a frame of its
//! own, and the body that builds it joins its set, because the value can run it.

use std::collections::{HashMap, HashSet};

use vyrn_frontend::ast::{FnId, Type, TypeDecl};
use vyrn_frontend::floor;
use vyrn_frontend::own::StateCallees;

use vyrn_frontend::core::{rows, Arg, Body, Made, Name, Place, Rhs, St};

/// The lattice's table lives in `vyrn_frontend::effects` because the
/// generation fence reads it mid-check and cannot see this crate.
pub use vyrn_frontend::effects::{
    atom, atoms, extern_effect, gen_allows, gen_refusal, Call, Effect, Effects, Walked,
    GEN_ATOM_OVERRIDES,
};

/// The callee names with no effect of their own and no body to judge: a
/// builtin row without an atom, a sum or record constructor, and a `std/mem`
/// or `std/runtime` declaration. A resolver asks [`atom`] and its bodies first.
pub struct PureNames<'a> {
    decls: &'a HashMap<String, TypeDecl>,
    variants: HashSet<&'a str>,
}

impl<'a> PureNames<'a> {
    pub fn new(decls: &'a HashMap<String, TypeDecl>) -> Self {
        let variants = decls
            .values()
            .filter_map(|d| match &d.base {
                Type::Enum(vs) => Some(vs),
                _ => None,
            })
            .flat_map(|vs| vs.iter().map(|v| v.name.as_str()))
            .collect();
        PureNames { decls, variants }
    }

    pub fn contains(&self, name: &str) -> bool {
        vyrn_frontend::prelude::builtin(name).is_some()
            || vyrn_frontend::checker::RESERVED.contains(&name)
            || name.starts_with('@')
            || name.starts_with(vyrn_frontend::loader::MEM_PREFIX)
            || name.starts_with(vyrn_frontend::loader::RUNTIME_PREFIX)
            || self.variants.contains(name)
            || self.decls.contains_key(name)
    }
}

/// What a callee's name resolves to. The caller of [`judge`] resolves names;
/// the judgment resolves nothing.
#[derive(Debug, Clone)]
pub enum Callee {
    /// A builtin or host import with a known effect.
    Atom(Effects),
    /// User bodies, by index into the slice handed to [`judge`]: several for
    /// several instances, impls or function-value sources.
    Bodies(Vec<usize>),
    /// A builtin with no effect.
    Pure,
    /// A call through a function value whose closed set is empty, so the call
    /// cannot run. Judged as pure and tallied apart from [`Callee::Unknown`].
    Empty,
    /// A name the caller cannot attribute. Judged as pure and reported.
    Unknown,
}

/// The judgment's answer for a set of bodies.
#[derive(Debug, Default)]
pub struct Judged<'a> {
    /// Per body, in the order given.
    pub effects: Vec<Effects>,
    /// `(body index, callee name, call line)` for every call nobody could
    /// attribute.
    pub unknown: Vec<(usize, String, usize)>,
    /// The same, for every [`Callee::Empty`] call.
    pub empty: Vec<(usize, String, usize)>,
    /// `(body index, callee name)` for every call through a function value
    /// that `through` answered with bodies.
    pub through: Vec<(usize, String)>,
    /// Per body, what the judgment read of it.
    pub frames: &'a [&'a Walked],
    /// Per body, each call's index into `callees`.
    resolved: Vec<Vec<usize>>,
    /// Every callee the calls resolved to, once.
    callees: Vec<Callee>,
    /// Per body, the globals it or a callee stores into, joined in the same
    /// fixpoint.
    writes: Vec<std::collections::BTreeSet<String>>,
}

impl Judged<'_> {
    /// Each callee name of body `i` with the globals a call to it may store
    /// into; a callee that stores into none is left out.
    pub fn state_callees(&self, i: usize) -> Vec<(String, Vec<String>)> {
        let mut out: Vec<(String, Vec<String>)> = Vec::new();
        for (c, k) in self.frames[i].calls.iter().zip(&self.resolved[i]) {
            let Callee::Bodies(idx) = &self.callees[*k] else {
                continue;
            };
            let gs: std::collections::BTreeSet<&String> =
                idx.iter().flat_map(|j| &self.writes[*j]).collect();
            if !gs.is_empty() && !out.iter().any(|(m, _)| *m == c.callee) {
                out.push((c.callee.clone(), gs.into_iter().cloned().collect()));
            }
        }
        out
    }

    /// [`Judged::state_callees`] of every frame of `refs`, by frame id. A
    /// frame whose callees store into nothing is left out, and so is a frame
    /// without a row, which no reader can ask for.
    pub(crate) fn state_table(&self, refs: &[&Body]) -> StateCallees {
        (refs.iter().enumerate())
            .filter_map(|(i, b)| Some((b.id?, self.state_callees(i))))
            .filter(|(_, cs)| !cs.is_empty())
            .collect()
    }
}

/// The globals a call to `callee` in the frame `frame` may store into, by
/// `state`, the effect judgment of the program being placed. A frame without
/// a row has none. The kernel ends every borrow of one of them at the call.
pub fn writes_state(state: &StateCallees, frame: Option<FnId>, callee: &str) -> Vec<String> {
    (frame.and_then(|f| state.get(&f)))
        .and_then(|cs| cs.iter().find(|(c, _)| c == callee))
        .map(|(_, gs)| gs.clone())
        .unwrap_or_default()
}

/// Walks every frame of `bodies`, on every thread. A body's lambdas are
/// joined only when their frames are in `bodies`, and each comes after the
/// frame that builds it, as [`Body::frames`] lists them.
pub fn walk_frames(bodies: &[&Body]) -> Vec<Walked> {
    let mut out = vyrn_frontend::par::in_parallel(
        bodies,
        // A frame's walk takes about 3 us on `site/export.vyrn`; each weighs one.
        |_| 1,
        || (),
        |_, b| {
            let mut w = Walk {
                body: b,
                out: Walked {
                    name: b.name.clone(),
                    own: Effects::PURE,
                    writes: Default::default(),
                    calls: Vec::new(),
                    lambdas: Vec::new(),
                },
            };
            rows(&b.stmts).for_each(|(s, _)| w.stmt(s));
            for info in b.names.iter().filter(|i| !i.borrow) {
                for r in &info.runs {
                    w.call(r, None, info.line, false);
                }
            }
            w.out
        },
    );
    let index: HashMap<*const Body, usize> = bodies
        .iter()
        .enumerate()
        .map(|(i, b)| (*b as *const Body, i))
        .collect();
    for (i, (b, walked)) in bodies.iter().zip(&mut out).enumerate() {
        walked.lambdas = (b.lambdas.iter())
            .filter_map(|l| index.get(&(l as *const Body)))
            .map(|j| {
                j.checked_sub(i)
                    .expect("a lambda frame comes after the frame that builds it")
            })
            .collect();
    }
    out
}

/// Returns the effect set of every frame in `frames`, to a fixpoint.
/// `resolve` says what a callee name is; `through` says what a function type
/// may hold, for a call through a local.
pub fn judge<'a>(
    frames: &'a [&'a Walked],
    resolve: &mut dyn FnMut(&str) -> Callee,
    through: &mut dyn FnMut(&Type) -> Callee,
) -> Judged<'a> {
    let mut own: Vec<Effects> = Vec::with_capacity(frames.len());
    let mut edges: Vec<Vec<usize>> = Vec::with_capacity(frames.len());
    let mut unknown = Vec::new();
    let mut empty = Vec::new();
    let mut via = Vec::new();
    let mut resolved = Vec::with_capacity(frames.len());
    let mut callees: Vec<Callee> = Vec::new();
    let mut named: HashMap<&str, usize> = HashMap::new();
    let mut typed: HashMap<&Type, usize> = HashMap::new();
    for (i, f) in frames.iter().enumerate() {
        let mut e = f.own;
        let mut to: Vec<usize> = Vec::new();
        let mut ids = Vec::with_capacity(f.calls.len());
        for c in &f.calls {
            // A call through a value reaches what the value's type may hold;
            // any other callee is resolved by name.
            let k = match &c.through {
                None => *named.entry(c.callee.as_str()).or_insert_with(|| {
                    callees.push(resolve(&c.callee));
                    callees.len() - 1
                }),
                Some(ty) => {
                    let k = *typed.entry(ty).or_insert_with(|| {
                        callees.push(through(ty));
                        callees.len() - 1
                    });
                    if matches!(callees[k], Callee::Bodies(_)) {
                        via.push((i, c.callee.clone()));
                    }
                    k
                }
            };
            let callee = &callees[k];
            match callee {
                Callee::Atom(a) => e = e.join(*a),
                Callee::Bodies(idx) => to.extend(idx.iter().copied()),
                Callee::Pure => {}
                Callee::Empty => empty.push((i, c.callee.clone(), c.line)),
                Callee::Unknown => unknown.push((i, c.callee.clone(), c.line)),
            }
            // An owned result of a call that is no user body is an
            // allocation; a user callee's own set answers for it.
            if c.born && !matches!(callee, Callee::Bodies(_)) {
                e = e.with(Effect::Alloc);
            }
            ids.push(k);
        }
        // The body that builds a lambda value can run its frame.
        to.extend(f.lambdas.iter().map(|d| i + d));
        to.sort_unstable();
        to.dedup();
        own.push(e);
        edges.push(to);
        resolved.push(ids);
    }
    let writes: Vec<std::collections::BTreeSet<String>> =
        frames.iter().map(|f| f.writes.clone()).collect();
    // Effect sets and the program's globals are finite, so the joins end.
    let solved = crate::fixpoint::solve(
        own.into_iter().zip(writes).collect(),
        &edges,
        |i, v| {
            let mut out = (Effects::PURE, std::collections::BTreeSet::new());
            for &j in &edges[i] {
                out.0 = out.0.join(v[j].0);
                out.1.extend(v[j].1.iter().cloned());
            }
            out
        },
        |old, (e, w), _| {
            let before = (old.0, old.1.len());
            old.0 = old.0.join(e);
            old.1.extend(w);
            (old.0, old.1.len()) != before
        },
    );
    let (effects, writes) = solved.into_iter().unzip();
    Judged {
        effects,
        unknown,
        empty,
        through: via,
        frames,
        resolved,
        callees,
        writes,
    }
}

fn global_root(p: &Place) -> Option<&String> {
    match p {
        Place::Global(g) => Some(g),
        Place::Name(_) => None,
        Place::Field(b, _) | Place::Elem(b, _) | Place::Key(b, _) => global_root(b),
    }
}

/// One frame's walk, which resolves no callee.
struct Walk<'a> {
    body: &'a Body,
    out: Walked,
}

impl Walk<'_> {
    fn stmt(&mut self, s: &St) {
        match s {
            St::Let(n, rhs) => {
                let line = self.body.names[n.index()].line;
                let made = rhs.allocates(&self.body.names, Some(*n));
                self.rhs(rhs, line, made.is_some());
                if let Rhs::Read(p) | Rhs::Take(p) = rhs {
                    self.place(p);
                }
                // An owned name born of a primitive or a literal is an
                // allocation; one born of a call is judged at the call.
                if matches!(made, Some(Made::Prim(_) | Made::Make(_))) {
                    self.out.own = self.out.own.with(Effect::Alloc);
                }
            }
            St::Do { rhs, line, .. } => self.rhs(rhs, *line, false),
            St::Trap | St::Check(_) => self.out.own = self.out.own.with(Effect::Trap),
            St::Store { place, .. } => {
                if let Some(g) = global_root(place) {
                    self.out.writes.insert(g.clone());
                }
                self.place(place)
            }
            St::Drop(..)
            | St::Row { .. }
            | St::If { .. }
            | St::Loop { .. }
            | St::Block { .. }
            | St::Switch { .. }
            | St::Break { .. }
            | St::Continue { .. }
            | St::Return { .. } => {}
        }
    }

    /// A place rooted at a global is module state; one rooted at a name is the
    /// frame's own and has no effect.
    fn place(&mut self, p: &Place) {
        match p {
            Place::Global(_) => self.out.own = self.out.own.with(Effect::ModuleState),
            Place::Name(_) => {}
            Place::Field(b, _) | Place::Elem(b, _) | Place::Key(b, _) => self.place(b),
        }
    }

    /// Records `r` when it is a call; `born` when it binds an owned result.
    fn rhs(&mut self, r: &Rhs, line: usize, born: bool) {
        let Rhs::Call {
            callee, kind, args, ..
        } = r
        else {
            return;
        };
        // A place argument is the place the call writes.
        for (a, _) in args {
            if let Arg::Place(p) = a {
                if let Some(g) = global_root(p) {
                    self.out.writes.insert(g.clone());
                }
                self.place(p);
            }
        }
        self.call(callee, kind.value(), line, born);
    }

    /// Records a call to `callee`, through `value` when it is a function value.
    fn call(&mut self, callee: &str, value: Option<Name>, line: usize, born: bool) {
        self.out.calls.push(Call {
            callee: callee.to_string(),
            through: value.map(|n| self.body.names[n.index()].ty.clone()),
            line,
            born,
        });
    }
}

/// Returns the floor's judged capabilities each module of a checked `program`
/// reaches; the pipeline passes it to [`vyrn_frontend::floor::decide`].
///
/// A module reaches a capability when an instance declared in it does, as
/// the placer judged it ([`crate::World::reached`]). The floor keeps its own
/// carrier and line and drops the rows this does not confirm. A module-scope `let` and a `where` predicate have no instance, so
/// they are read from the AST with [`vyrn_frontend::floor::call_carrier`].
pub fn reaches(
    program: &vyrn_frontend::ast::Program,
    world: &crate::World,
) -> Vec<(String, floor::Capability)> {
    let mut out: Vec<(String, floor::Capability)> = Vec::new();
    let mut add = |module: Option<&String>, cap: floor::Capability| {
        let key = module.cloned().unwrap_or_default();
        if !out.iter().any(|(m, c)| *m == key && *c == cap) {
            out.push((key, cap));
        }
    };

    let externs = floor::extern_imports(program);
    let spells = |e: &vyrn_frontend::ast::Expr| -> Vec<floor::Capability> {
        let mut e = e.clone();
        let mut found: Vec<floor::Capability> = Vec::new();
        vyrn_frontend::project::walk_bare(&mut e, &mut |x| {
            if let vyrn_frontend::ast::Expr::Call { name, .. } = x {
                if let Some(cap) = floor::call_carrier(name, &externs) {
                    found.push(cap);
                }
            }
        });
        found
    };
    let mut declared: Vec<(Option<String>, floor::Capability)> = Vec::new();
    for g in &program.globals {
        declared.extend(spells(&g.init).into_iter().map(|c| (g.module.clone(), c)));
    }
    for t in &program.type_decls {
        if let Some(p) = &t.predicate {
            declared.extend(spells(p).into_iter().map(|c| (t.module.clone(), c)));
        }
    }
    for (module, cap) in declared {
        add(module.as_ref(), cap);
    }

    let rows: Vec<(Effect, floor::Capability)> = Effect::ALL
        .into_iter()
        .filter_map(|e| floor::Capability::of(e).map(|cap| (e, cap)))
        .collect();

    let placed;
    let reached = if world.late {
        placed = placed_reach(program, world);
        &placed
    } else {
        &world.reached
    };
    for (module, e) in reached {
        for (effect, cap) in &rows {
            if e.has(*effect) {
                add(module.as_ref(), *cap);
            }
        }
    }
    out
}

/// Judges `program` as the placer left it and returns each instance's set, as
/// [`crate::World::reached`] does. The placer judged before it placed, so a
/// release that only a placed row names (`World::late`) is in no judged set;
/// this analyzes the program again, lowers it with every row, and judges every
/// instance that builds.
fn placed_reach(
    program: &vyrn_frontend::ast::Program,
    world: &crate::World,
) -> Vec<(Option<String>, Effects)> {
    let world = crate::world::analyzed(program, world.ownership.record.clone(), false);
    let own = &world.ownership;
    let lowered = crate::lower_with(program, own);
    let mut bodies = Vec::new();
    let mut insts = Vec::new();
    for inst in lowered.instances.iter().filter(|i| !i.func.is_gen) {
        if let Ok(b) = crate::core::build(program, inst, own) {
            bodies.push(b);
            insts.push(inst);
        }
    }
    let tops: Vec<(&str, &Body)> = (insts.iter().zip(&bodies))
        .map(|(i, b)| (i.func.name.as_str(), b))
        .collect();
    let mut fns = crate::Fns::lowered(&lowered);
    let places = crate::core::build_places(program, &lowered, own, &mut fns);
    judge_built(
        program,
        &lowered,
        own,
        &mut fns,
        &places,
        &tops,
        &[],
        |_, reach, _, top, _| {
            (insts.iter().zip(top))
                .map(|(i, at)| (i.func.module.clone(), reach.effects[*at]))
                .collect()
        },
    )
}

/// The judgment over bodies the caller built: `tops` holds each body with
/// the name a call spells it by, and `served` each body the judgment memo
/// served, by its frames as last judged. `places` holds the projection bodies
/// ([`crate::core::build_places`]). The generic releases are built here, each
/// frame numbered in `fns` ([`crate::Fns::number`]), and `then` is given
/// every frame built in the order judged, `top[i]`, the frame index of
/// `tops[i]`'s own body, and `served_at[i]`, that of `served[i]`'s. Every
/// served frame comes after the last frame built.
///
/// `then` is also given a second judgment, `reach`, that no `test` or `bench`
/// body or lambda of one answers a call through a function value. The floor
/// reads it, because a lambda in a test is not in the artifact. It is the first
/// judgment itself when no call through a value reached such a frame.
pub(crate) fn judge_built<R>(
    program: &vyrn_frontend::ast::Program,
    lowered: &crate::Lowered<'_>,
    own: &vyrn_frontend::own::Ownership,
    fns: &mut crate::Fns,
    places: &[crate::core::Projection<'_, '_>],
    tops: &[(&str, &Body)],
    served: &[(&str, &[Walked])],
    then: impl FnOnce(&Judged, &Judged, &[&Body], &[usize], &[usize]) -> R,
) -> R {
    let mut generic: Vec<(&str, Body)> = Vec::new();
    // Nor does a generic declared `release` until the placer writes the row
    // that calls it, so it is judged as written.
    let generic_release = |f: &vyrn_frontend::ast::Function| {
        !f.type_params.is_empty() && own.proto.is_release_fn(&f.name)
    };
    if program.functions.iter().any(generic_release) {
        let written = vyrn_frontend::own::Ownership {
            proto: own.proto.as_written(),
            ..own.clone()
        };
        for inst in crate::as_written(program, own) {
            if !generic_release(inst.func) {
                continue;
            }
            let built =
                crate::core::build_in(program, &inst, &written, fns, &mut Default::default());
            if let Ok(mut b) = built {
                fns.number(&mut b);
                generic.push((inst.func.name.as_str(), b));
            }
        }
    }
    // A lambda frame is keyed by its defining function, line and column, as
    // a lambda source is named.
    let mut refs: Vec<&Body> = Vec::new();
    let mut top: Vec<usize> = Vec::new();
    let mut lambda_frames: HashMap<(&str, (usize, usize)), Vec<usize>> = HashMap::new();
    for (name, b) in tops {
        for f in b.frames() {
            if std::ptr::eq(f, *b) {
                top.push(refs.len());
            } else if let Some(pos) = crate::core::lambda_at(&f.name) {
                lambda_frames
                    .entry((name, pos))
                    .or_default()
                    .push(refs.len());
            }
            refs.push(f);
        }
    }
    // The module-state initializer's lambdas are keyed under the empty
    // name.
    let mut state = crate::core::build_module_state(program, own, fns, &lowered.globals).ok();
    if let Some(b) = &mut state {
        fns.number(b);
    }
    for f in state.iter().flat_map(|b| b.frames()).skip(1) {
        if let Some(pos) = crate::core::lambda_at(&f.name) {
            lambda_frames.entry(("", pos)).or_default().push(refs.len());
        }
        refs.push(f);
    }
    let mut place_tops: HashMap<&str, Vec<usize>> = HashMap::new();
    // A projection has no instance but is a call by its own name in the core,
    // so it is judged too.
    let projected =
        (places.iter()).filter_map(|(p, b)| Some((p.func.name.as_str(), b.as_ref().ok()?)));
    for (name, b) in projected.chain(generic.iter().map(|(n, b)| (*n, b))) {
        place_tops.entry(name).or_default().push(refs.len());
        for f in b.frames() {
            refs.push(f);
        }
    }
    let mut by_name: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, (name, _)) in tops.iter().enumerate() {
        by_name.entry(name).or_default().push(top[i]);
    }
    let built = walk_frames(&refs);
    let mut frames: Vec<&Walked> = built.iter().collect();
    let mut served_at: Vec<usize> = Vec::with_capacity(served.len());
    for (name, walked) in served {
        let at = frames.len();
        served_at.push(at);
        by_name.entry(name).or_default().push(at);
        for (k, f) in walked.iter().enumerate().skip(1) {
            if let Some(pos) = crate::core::lambda_at(&f.name) {
                (lambda_frames.entry((name, pos)).or_default()).push(at + k);
            }
        }
        frames.extend(walked.iter());
    }
    let mut impl_methods: HashMap<&str, Vec<usize>> = HashMap::new();
    for im in &program.impls {
        for m in im.methods.iter().chain(im.places.iter()) {
            if let Some(key) = vyrn_frontend::types::type_key(&im.ty) {
                let mangled = vyrn_frontend::types::impl_method_name(&im.protocol, &key, &m.name);
                if let Some(idx) = by_name.get(mangled.as_str()) {
                    impl_methods
                        .entry(m.name.as_str())
                        .or_default()
                        .extend(idx.iter().copied());
                }
            }
        }
    }
    let decls = own.proto.types();
    let pure = PureNames::new(decls);
    let externs: std::collections::BTreeMap<&str, Effect> = (program.functions.iter())
        .filter_map(|f| Some((f.name.as_str(), extern_effect(f)?)))
        .collect();
    let mut resolve = |name: &str| -> Callee {
        if let Some(e) = atom(name) {
            return Callee::Atom(Effects::of(e));
        }
        if let Some(&e) = externs.get(name) {
            return Callee::Atom(Effects::of(e));
        }
        if let Some(idx) = by_name.get(name) {
            return Callee::Bodies(idx.clone());
        }
        if let Some(idx) = impl_methods.get(name) {
            return Callee::Bodies(idx.clone());
        }
        // A projection dispatched by name.
        if let Some(idx) = place_tops.get(name) {
            return Callee::Bodies(idx.clone());
        }
        if pure.contains(name) {
            return Callee::Pure;
        }
        Callee::Unknown
    };
    let stored = &own.record.stored;
    let outside: HashSet<&str> = lowered.bodies.iter().map(|b| b.name.as_str()).collect();
    let (skip, hit) = (std::cell::Cell::new(false), std::cell::Cell::new(false));
    let mut through = |ty: &Type| -> Callee {
        let ty = &vyrn_frontend::types::resolve(ty, decls);
        if !matches!(ty, Type::Fn(..)) {
            return Callee::Unknown;
        }
        let mut idx: Vec<usize> = Vec::new();
        for src in stored.every_source() {
            if !vyrn_frontend::checker::fn_sigs_match(&src.sig, ty) {
                continue;
            }
            if let Some(n) = &src.named {
                if let Some(i) = by_name.get(n.as_str()) {
                    idx.extend(i.iter().copied());
                }
            }
            if let Some(l) = &src.lambda {
                let out = outside.contains(l.defined_in.as_str());
                hit.set(hit.get() || out);
                if out && skip.get() {
                    continue;
                }
                if let Some(i) = lambda_frames.get(&(l.defined_in.as_str(), (l.line, l.col))) {
                    idx.extend(i.iter().copied());
                }
            }
        }
        idx.sort_unstable();
        idx.dedup();
        if idx.is_empty() {
            Callee::Empty
        } else {
            Callee::Bodies(idx)
        }
    };
    let judged = judge(&frames, &mut resolve, &mut through);
    let reach = hit.get().then(|| {
        skip.set(true);
        judge(&frames, &mut resolve, &mut through)
    });
    then(
        &judged,
        reach.as_ref().unwrap_or(&judged),
        &refs,
        &top,
        &served_at,
    )
}
