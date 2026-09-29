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

use vyrn_frontend::ast::{Type, TypeDecl};
use vyrn_frontend::floor;

use crate::core::{rows, Body, Place, Rhs, St};

/// The lattice's table lives in `vyrn_frontend::effects` because the
/// generation fence reads it mid-check and cannot see this crate.
pub use vyrn_frontend::effects::{
    atom, atoms, gen_allows, gen_refusal, Effect, Effects, GEN_ATOM_OVERRIDES,
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
pub struct Judged {
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
    calls: Vec<Vec<(String, Callee)>>,
    /// Per body, the globals it or a callee stores into, joined in the same
    /// fixpoint.
    writes: Vec<std::collections::BTreeSet<String>>,
}

impl Judged {
    /// Each callee name of body `i` with the globals a call to it may store
    /// into; a callee that stores into none is left out.
    pub fn state_callees(&self, i: usize) -> Vec<(String, Vec<String>)> {
        let mut out: Vec<(String, Vec<String>)> = Vec::new();
        for (n, c) in &self.calls[i] {
            let Callee::Bodies(idx) = c else {
                continue;
            };
            let gs: std::collections::BTreeSet<&String> =
                idx.iter().flat_map(|j| &self.writes[*j]).collect();
            if !gs.is_empty() && !out.iter().any(|(m, _)| m == n) {
                out.push((n.clone(), gs.into_iter().cloned().collect()));
            }
        }
        out
    }
}

thread_local! {
    /// [`Judged::state_callees`] by body name, for the program `augment` is
    /// placing; empty outside it.
    static STATE_CALLEES: std::cell::RefCell<HashMap<String, Vec<(String, Vec<String>)>>> =
        std::cell::RefCell::new(HashMap::new());
}

/// The globals a call to `callee` in the body named `body` may store into,
/// by the judgment of the program being placed. The kernel ends every borrow
/// of one of them at the call.
pub fn writes_state(body: &str, callee: &str) -> Vec<String> {
    STATE_CALLEES.with(|m| {
        m.borrow()
            .get(body)
            .and_then(|cs| cs.iter().find(|(c, _)| c == callee))
            .map(|(_, gs)| gs.clone())
            .unwrap_or_default()
    })
}

/// Whether the body named `body` calls a function that stores into module
/// state, by the judgment of the program being placed.
pub fn stores_state(body: &str) -> bool {
    STATE_CALLEES.with(|m| m.borrow().contains_key(body))
}

/// Record the judgment's module-state callees for every frame of `refs`.
/// `None` clears them.
pub(crate) fn set_state_callees(judged: Option<(&Judged, &[&Body])>) {
    let map = judged
        .map(|(j, refs)| {
            refs.iter()
                .enumerate()
                .map(|(i, b)| (b.name.clone(), j.state_callees(i)))
                .filter(|(_, cs)| !cs.is_empty())
                .collect()
        })
        .unwrap_or_default();
    STATE_CALLEES.with(|m| *m.borrow_mut() = map);
}

/// Returns the effect set of every body in `bodies`, to a fixpoint. `resolve`
/// says what a callee name is; `through` says what a function type may hold,
/// for a call through a local. A body's lambdas are joined only when their
/// frames are in `bodies`.
pub fn judge(
    bodies: &[&Body],
    resolve: &mut dyn FnMut(&str) -> Callee,
    through: &mut dyn FnMut(&Type) -> Callee,
) -> Judged {
    let index: HashMap<*const Body, usize> = bodies
        .iter()
        .enumerate()
        .map(|(i, b)| (*b as *const Body, i))
        .collect();
    let mut own: Vec<Effects> = Vec::with_capacity(bodies.len());
    let mut edges: Vec<Vec<usize>> = Vec::with_capacity(bodies.len());
    let mut unknown = Vec::new();
    let mut empty = Vec::new();
    let mut via = Vec::new();
    let mut calls = Vec::with_capacity(bodies.len());
    let mut writes: Vec<std::collections::BTreeSet<String>> = Vec::with_capacity(bodies.len());
    let mut memo: HashMap<String, Callee> = HashMap::new();
    let mut memo_ty: HashMap<String, Callee> = HashMap::new();
    for (i, b) in bodies.iter().enumerate() {
        let mut w = Walk {
            body: b,
            own: Effects::PURE,
            edges: Vec::new(),
            unknown: Vec::new(),
            empty: Vec::new(),
            via: Vec::new(),
            calls: Vec::new(),
            writes: Default::default(),
            resolve,
            through,
            memo: &mut memo,
            memo_ty: &mut memo_ty,
        };
        rows(&b.stmts).for_each(|(s, _)| w.stmt(s));
        for info in b.names.iter().filter(|i| !i.borrow) {
            for r in &info.runs {
                w.call(r, None, info.line);
            }
        }
        own.push(w.own);
        let mut e = w.edges;
        // The body that builds a lambda value can run its frame.
        e.extend(
            b.lambdas
                .iter()
                .filter_map(|l| index.get(&(l as *const Body))),
        );
        e.sort_unstable();
        e.dedup();
        edges.push(e);
        unknown.extend(w.unknown.into_iter().map(|(n, l)| (i, n, l)));
        empty.extend(w.empty.into_iter().map(|(n, l)| (i, n, l)));
        via.extend(w.via.into_iter().map(|n| (i, n)));
        calls.push(w.calls);
        writes.push(w.writes);
    }
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
        calls,
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

struct Walk<'a> {
    body: &'a Body,
    own: Effects,
    edges: Vec<usize>,
    unknown: Vec<(String, usize)>,
    empty: Vec<(String, usize)>,
    /// The callees `through` answered with bodies.
    via: Vec<String>,
    calls: Vec<(String, Callee)>,
    writes: std::collections::BTreeSet<String>,
    resolve: &'a mut dyn FnMut(&str) -> Callee,
    through: &'a mut dyn FnMut(&Type) -> Callee,
    memo: &'a mut HashMap<String, Callee>,
    memo_ty: &'a mut HashMap<String, Callee>,
}

impl Walk<'_> {
    fn stmt(&mut self, s: &St) {
        match s {
            St::Let(n, rhs) => {
                let atom_call = self.rhs(rhs, self.body.names[*n as usize].line);
                if let Rhs::Read(p) | Rhs::Take(p) = rhs {
                    self.place(p);
                }
                // An owned name born of a primitive, a literal or a builtin
                // is an allocation; a user callee's own set answers for it.
                let born = match rhs {
                    Rhs::Prim(..) | Rhs::Make(..) => true,
                    Rhs::Call { .. } => atom_call,
                    Rhs::Val(_) | Rhs::Read(_) | Rhs::Take(_) => false,
                };
                if born && self.body.names[*n as usize].releases {
                    self.own = self.own.with(Effect::Alloc);
                }
            }
            St::Do { rhs, line, .. } => {
                self.rhs(rhs, *line);
            }
            St::Trap | St::Check(_) => self.own = self.own.with(Effect::Trap),
            St::Store { place, .. } => {
                if let Some(g) = global_root(place) {
                    self.writes.insert(g.clone());
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
            Place::Global(_) => self.own = self.own.with(Effect::ModuleState),
            Place::Name(_) => {}
            Place::Field(b, _) | Place::Elem(b, _) | Place::Key(b, _) => self.place(b),
        }
    }

    /// Who a call reaches. A call through a value (`value`) reaches what the
    /// value's type may hold; any other callee is resolved by name.
    fn callee(&mut self, callee: &str, value: Option<crate::core::Name>) -> Callee {
        let Some(n) = value else {
            return match self.memo.get(callee) {
                Some(c) => c.clone(),
                None => {
                    let c = (self.resolve)(callee);
                    self.memo.insert(callee.to_string(), c.clone());
                    c
                }
            };
        };
        let ty = &self.body.names[n as usize].ty;
        let key = ty.to_string();
        let c = match self.memo_ty.get(&key) {
            Some(c) => c.clone(),
            None => {
                let c = (self.through)(ty);
                self.memo_ty.insert(key, c.clone());
                c
            }
        };
        if matches!(c, Callee::Bodies(_)) {
            self.via.push(callee.to_string());
        }
        c
    }

    /// Whether `r` is a call that is not a user body: the caller's own
    /// allocation when the result is owned.
    fn rhs(&mut self, r: &Rhs, line: usize) -> bool {
        let Rhs::Call {
            callee, kind, args, ..
        } = r
        else {
            return false;
        };
        // A place argument is a move-out window's place; the call writes it.
        for (a, _) in args {
            if let crate::core::Arg::Place(p) = a {
                if let Some(g) = global_root(p) {
                    self.writes.insert(g.clone());
                }
                self.place(p);
            }
        }
        self.call(callee, kind.value(), line)
    }

    /// Joins a call to `callee` (through `value` when it is a function value);
    /// true when the callee is not a user body.
    fn call(&mut self, callee: &str, value: Option<crate::core::Name>, line: usize) -> bool {
        let c = self.callee(callee, value);
        self.calls.push((callee.to_string(), c.clone()));
        match c {
            Callee::Atom(e) => {
                self.own = self.own.join(e);
                true
            }
            Callee::Bodies(idx) => {
                self.edges.extend(idx.iter().copied());
                false
            }
            Callee::Pure => true,
            Callee::Empty => {
                self.empty.push((callee.to_string(), line));
                true
            }
            Callee::Unknown => {
                self.unknown.push((callee.to_string(), line));
                true
            }
        }
    }
}

/// Returns the floor's judged capabilities each module of a checked `program`
/// reaches; installed as [`vyrn_frontend::floor::Judge`].
///
/// A module reaches a capability when an instance declared in it does. The
/// floor keeps its own carrier and line and drops the rows this does not
/// confirm. A module-scope `let` and a `where` predicate have no instance, so
/// they are read from the AST with [`vyrn_frontend::floor::call_carrier`].
pub fn reaches(program: &vyrn_frontend::ast::Program) -> Vec<(String, floor::Capability)> {
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

    with_judgment(program, |judged, _refs, insts, top| {
        for (i, inst) in insts.iter().enumerate() {
            // A `gen fn` runs at generation time and is never in the artifact,
            // so it reaches no capability of the target; `floor::carried` skips
            // it too. The fence judges generators.
            if inst.func.is_gen {
                continue;
            }
            let e = judged.effects[top[i]];
            for (effect, cap) in &rows {
                if e.has(*effect) {
                    add(inst.func.module.as_ref(), *cap);
                }
            }
        }
    });
    out
}

/// Hands `then` the judgment over a whole checked program, every frame in the
/// order judged, the instances that have a core, and `top[i]`, the frame index
/// of instance `i`'s own body. A callback, because `refs` borrows `bodies`.
fn with_judgment<R>(
    program: &vyrn_frontend::ast::Program,
    then: impl FnOnce(&Judged, &[&crate::core::Body], &[&crate::Instance], &[usize]) -> R,
) -> R {
    let lowered = crate::lower(program);
    let own = vyrn_frontend::own::analyze(program);
    let mut bodies = Vec::new();
    let mut insts = Vec::new();
    for inst in &lowered.instances {
        if let Ok(b) = crate::core::build(program, inst, &own) {
            bodies.push(b);
            insts.push(inst);
        }
    }
    let tops: Vec<(&str, &crate::core::Body)> = insts
        .iter()
        .zip(&bodies)
        .map(|(i, b)| (i.func.name.as_str(), b))
        .collect();
    judge_built(program, &lowered, &own, &tops, |judged, refs, top| {
        then(judged, refs, &insts, top)
    })
}

/// The judgment over bodies the caller built: `tops` holds each body with
/// the name a call spells it by. The projection bodies are built here, and
/// `then` is given every frame in the order judged and `top[i]`, the frame
/// index of `tops[i]`'s own body.
pub(crate) fn judge_built<R>(
    program: &vyrn_frontend::ast::Program,
    lowered: &crate::Lowered<'_>,
    own: &vyrn_frontend::own::Ownership,
    tops: &[(&str, &crate::core::Body)],
    then: impl FnOnce(&Judged, &[&crate::core::Body], &[usize]) -> R,
) -> R {
    // An `impl` projection has no instance but is a call by its own name in
    // the core, so it is judged too.
    let mut place_bodies: Vec<(&str, crate::core::Body)> = Vec::new();
    for pr in &lowered.places {
        let inst = crate::Instance {
            func: pr.func,
            type_args: Vec::new(),
            subst: Default::default(),
            facts: pr.facts.clone(),
            releases: Vec::new(),
        };
        if let Ok(b) = crate::core::build(program, &inst, own) {
            place_bodies.push((pr.func.name.as_str(), b));
        }
    }
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
            if let Ok(b) = crate::core::build(program, &inst, &written) {
                place_bodies.push((inst.func.name.as_str(), b));
            }
        }
    }
    // A lambda frame is keyed by its defining function and line, as a
    // lambda source is named.
    let mut refs: Vec<&crate::core::Body> = Vec::new();
    let mut top: Vec<usize> = Vec::new();
    let mut lambda_frames: HashMap<(&str, usize), Vec<usize>> = HashMap::new();
    for (name, b) in tops {
        for f in b.frames() {
            if std::ptr::eq(f, *b) {
                top.push(refs.len());
            } else if let Some(line) = crate::core::lambda_line(&f.name) {
                lambda_frames
                    .entry((name, line))
                    .or_default()
                    .push(refs.len());
            }
            refs.push(f);
        }
    }
    // The module-state initializer's lambdas are keyed under the empty
    // name.
    let state = crate::core::build_module_state(program, own, &lowered.globals).ok();
    for f in state.iter().flat_map(|b| b.frames()).skip(1) {
        if let Some(line) = crate::core::lambda_line(&f.name) {
            lambda_frames
                .entry(("", line))
                .or_default()
                .push(refs.len());
        }
        refs.push(f);
    }
    let mut place_tops: HashMap<&str, Vec<usize>> = HashMap::new();
    for (name, b) in &place_bodies {
        place_tops.entry(name).or_default().push(refs.len());
        for f in b.frames() {
            refs.push(f);
        }
    }
    let mut by_name: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, (name, _)) in tops.iter().enumerate() {
        by_name.entry(name).or_default().push(top[i]);
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
    let externs: std::collections::BTreeSet<&str> = program
        .functions
        .iter()
        .filter(|f| f.is_extern)
        .map(|f| f.name.as_str())
        .collect();
    let mut resolve = |name: &str| -> Callee {
        if let Some(e) = atom(name) {
            return Callee::Atom(Effects::of(e));
        }
        if externs.contains(name) {
            return Callee::Atom(Effects::of(Effect::Extern));
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
    let stored = vyrn_frontend::checker::stored_fn_effects(program);
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
                if let Some(i) = lambda_frames.get(&(l.defined_in.as_str(), l.line)) {
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
    let judged = judge(&refs, &mut resolve, &mut through);
    then(&judged, &refs, &top)
}
