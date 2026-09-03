//! The effect judgment — RFC-0125 §2.2, judgment 2 (M6, first slice).
//!
//! Over a [`Body`] in the named core: **a body's effect set is the join of
//! its own atoms and its callees' sets**, taken to a fixpoint so recursion
//! is handled. An atom is a builtin or a host import the runtime declares;
//! the lattice is stated ONCE, as the table in RFC-0125 §3 M6, and
//! [`ATOMS`] here is that table's second column. `tests/effects.rs` reads
//! the table out of the RFC and refuses to run if the two differ, which is
//! the direction the RFC asks for: the code derives from the table.
//!
//! The judgment knows nothing about the surface language. It sees a call by
//! its callee's name, the spawn marker on a call (`Rhs::Call::spawn`), an
//! owned name born of a primitive or a literal (an allocation), and a trap.
//! A call through a function value names a local, and the caller answers
//! for its type with the closed set of functions a value of that type may
//! hold — RFC-0037's stored sources (finding 3 of the first slice). A lambda
//! body is a frame of its own (RFC-0125 M3) and the caller hands every frame
//! in; a body joins the frames of the lambdas it holds, because the value it
//! builds can run them (finding 2).
//!
//! One inclusion check lives here: the spawn-isolation rule of RFC-0004 §Q4,
//! stated as RFC-0125 §2.2 states it — a spawned callee's set within
//! [`Effects::SPAWN_ALLOWS`]. The other two, a body within its target's set
//! and within its context's, still wait on the floor and the fence;
//! `tests/effects.rs` compares the set with both passes, per function.

use std::collections::HashMap;

use vyrn_frontend::ast::Type;
use vyrn_frontend::floor;

use crate::core::{Body, Rhs, St};

/// The lattice's table — the effects, the sets and [`ATOMS`] — lives in
/// `vyrn_frontend::effects`, because RFC-0021's generation fence reads the
/// same table mid-check and cannot see this crate (RFC-0125 §3 M6, fifth
/// slice). Re-exported so the judgment still spells it `effects::`.
pub use vyrn_frontend::effects::{
    atom, gen_allows, gen_refusal, Effect, Effects, ATOMS, GEN_ATOM_OVERRIDES,
};

/// What a callee's name resolves to, as the caller of [`judge`] sees the
/// program. The judgment itself resolves nothing: it does not know which
/// names are functions, which are `extern`, which are builtins, and which
/// function values a type may hold.
#[derive(Debug, Clone)]
pub enum Callee {
    /// A builtin or host import with a known effect.
    Atom(Effects),
    /// User bodies, by index into the slice handed to [`judge`] — several
    /// when a name has several instances or several impls, or when a
    /// function type has several sources (RFC-0037).
    Bodies(Vec<usize>),
    /// A builtin with no effect.
    Pure,
    /// A name the caller cannot attribute, or a function type with no known
    /// source. Judged as pure and reported, so the tally counts it.
    Unknown,
}

/// One `spawn` the judgment saw.
#[derive(Debug, Clone)]
pub struct Spawned {
    /// The body the spawn is in, by index.
    pub body: usize,
    pub callee: String,
    pub line: usize,
    /// The spawned callee's set, after the fixpoint.
    pub effects: Effects,
}

impl Spawned {
    /// What the spawn-isolation rule refuses: the callee's effects outside
    /// [`Effects::SPAWN_ALLOWS`]. Pure when the spawn is allowed.
    pub fn outside(&self) -> Effects {
        self.effects.minus(Effects::SPAWN_ALLOWS)
    }
}

/// The judgment's answer for a set of bodies.
#[derive(Debug, Default)]
pub struct Judged {
    /// Per body, in the order given.
    pub effects: Vec<Effects>,
    /// `(body index, callee name, line)` for every call nobody could
    /// attribute. The line is the call's, so a remaining one can be read in
    /// the source (RFC-0125 §3 M6, finding 14).
    pub unknown: Vec<(usize, String, usize)>,
    /// `(body index, callee name)` for every call through a function value
    /// that `through` answered with bodies.
    pub through: Vec<(usize, String)>,
    /// Every spawn site, with its callee's set.
    pub spawns: Vec<Spawned>,
}

/// The effect set of every body in `bodies`, each the join of its own atoms,
/// its callees' and its lambda frames', to a fixpoint. `resolve` says what
/// a callee name is; `through` says what a function TYPE may hold, for a
/// call whose callee is a name of the body (a parameter or a binding of
/// function type). `bodies` holds every frame the caller wants judged: a
/// body's lambdas are joined only when they are in the slice.
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
    let mut via = Vec::new();
    let mut spawns: Vec<(usize, String, usize, Vec<usize>)> = Vec::new();
    // One resolution per distinct name and per distinct type, not one per
    // call site.
    let mut memo: HashMap<String, Callee> = HashMap::new();
    let mut memo_ty: HashMap<String, Callee> = HashMap::new();
    for (i, b) in bodies.iter().enumerate() {
        let mut w = Walk {
            body: b,
            own: Effects::PURE,
            edges: Vec::new(),
            unknown: Vec::new(),
            via: Vec::new(),
            spawns: Vec::new(),
            resolve,
            through,
            memo: &mut memo,
            memo_ty: &mut memo_ty,
        };
        w.stmts(&b.stmts);
        own.push(w.own);
        let mut e = w.edges;
        // A lambda's frame runs when the value is invoked, and the body that
        // built the value is where the invocation is possible: presence, as
        // the floor counts it.
        e.extend(
            b.lambdas
                .iter()
                .filter_map(|l| index.get(&(l as *const Body))),
        );
        e.sort_unstable();
        e.dedup();
        edges.push(e);
        unknown.extend(w.unknown.into_iter().map(|(n, l)| (i, n, l)));
        via.extend(w.via.into_iter().map(|n| (i, n)));
        spawns.extend(w.spawns.into_iter().map(|(c, l, idx)| (i, c, l, idx)));
    }
    // The fixpoint. Monotone over a finite lattice, so it ends; the corpus
    // needs a handful of rounds.
    let mut effects = own.clone();
    loop {
        let mut changed = false;
        for i in 0..bodies.len() {
            let mut e = effects[i];
            for &j in &edges[i] {
                e = e.join(effects[j]);
            }
            if e != effects[i] {
                effects[i] = e;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let spawns = spawns
        .into_iter()
        .map(|(body, callee, line, idx)| Spawned {
            body,
            callee,
            line,
            effects: idx
                .iter()
                .map(|j| effects[*j])
                .fold(Effects::PURE, Effects::join),
        })
        .collect();
    Judged {
        effects,
        unknown,
        through: via,
        spawns,
    }
}

struct Walk<'a> {
    body: &'a Body,
    own: Effects,
    edges: Vec<usize>,
    unknown: Vec<(String, usize)>,
    /// The callees `through` answered with bodies.
    via: Vec<String>,
    /// `(callee, line, callee bodies)` per spawn.
    spawns: Vec<(String, usize, Vec<usize>)>,
    resolve: &'a mut dyn FnMut(&str) -> Callee,
    through: &'a mut dyn FnMut(&Type) -> Callee,
    memo: &'a mut HashMap<String, Callee>,
    memo_ty: &'a mut HashMap<String, Callee>,
}

impl Walk<'_> {
    fn stmts(&mut self, stmts: &[St]) {
        for s in stmts {
            self.stmt(s);
        }
    }

    fn stmt(&mut self, s: &St) {
        match s {
            St::Let(n, rhs) => {
                let atom_call = self.rhs(rhs, self.body.names[*n as usize].line);
                // An owned name born of a primitive, a literal or a builtin
                // is an allocation. Born of a user call, the callee's own
                // set says whether it allocated or handed a parameter back.
                let born = match rhs {
                    Rhs::Prim(_) | Rhs::Make(_) => true,
                    Rhs::Call { .. } => atom_call,
                    Rhs::Val(_) | Rhs::Read(_) | Rhs::Take(_) => false,
                };
                if born && self.body.names[*n as usize].owned {
                    self.own = self.own.with(Effect::Alloc);
                }
            }
            St::Do(rhs, line) => {
                self.rhs(rhs, *line);
            }
            St::Trap => self.own = self.own.with(Effect::Trap),
            St::If { then, els, .. } => {
                self.stmts(then);
                self.stmts(els);
            }
            St::Loop(body) | St::Block { body, .. } => self.stmts(body),
            St::Switch { arms, .. } => {
                for a in arms {
                    self.stmts(&a.body);
                }
            }
            St::Store { .. }
            | St::Drop(_)
            | St::Row { .. }
            | St::Break { .. }
            | St::Continue { .. }
            | St::Return { .. } => {}
        }
    }

    /// What `callee` is: by name first, and for a name of this body, by its
    /// type — a callable local holds a function value, whatever alias the
    /// type is spelled through.
    fn callee(&mut self, callee: &str) -> Callee {
        let c = match self.memo.get(callee) {
            Some(c) => c.clone(),
            None => {
                let c = (self.resolve)(callee);
                self.memo.insert(callee.to_string(), c.clone());
                c
            }
        };
        if !matches!(c, Callee::Unknown) {
            return c;
        }
        let Some(ty) = self
            .body
            .names
            .iter()
            .find(|n| n.source == callee)
            .map(|n| &n.ty)
        else {
            return Callee::Unknown;
        };
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

    /// Whether the right-hand side was a call that is not a user body — the
    /// caller's own allocation when the result is owned.
    fn rhs(&mut self, r: &Rhs, line: usize) -> bool {
        let Rhs::Call { callee, spawn, .. } = r else {
            return false;
        };
        if *spawn {
            self.own = self.own.with(Effect::Spawn);
        }
        let c = self.callee(callee);
        let (atom, idx) = match c {
            Callee::Atom(e) => {
                self.own = self.own.join(e);
                (true, Vec::new())
            }
            Callee::Bodies(idx) => {
                self.edges.extend(idx.iter().copied());
                (false, idx)
            }
            Callee::Pure => (true, Vec::new()),
            Callee::Unknown => {
                self.unknown.push((callee.clone(), line));
                (true, Vec::new())
            }
        };
        if *spawn {
            self.spawns.push((callee.clone(), line, idx));
        }
        atom
    }
}

/// Which of the floor's judged capabilities each module of `program` REACHES —
/// RFC-0125 §3 M6, fourth slice.
///
/// This is the judgment installed as [`vyrn_frontend::floor::Judge`]. It is
/// handed a checked program, builds the named core for every instance the
/// program has, and joins each body's set to a fixpoint; a module reaches a
/// capability when an instance declared in it does. The floor keeps the words
/// — the carrier its scan found and the line it found it on — and drops the
/// rows this does not confirm, so the rule for these rows becomes reachability
/// where the rest of the floor stays presence (finding 8).
///
/// A `let` at module scope (RFC-0013) and a `where` predicate are declarations
/// the instance list does not cover, so they are read here from the AST by the
/// same [`vyrn_frontend::floor::JUDGED`] names. Nothing else in the program can
/// carry one of these rows.
pub fn reaches(program: &vyrn_frontend::ast::Program) -> Vec<(String, floor::Capability)> {
    let mut out: Vec<(String, floor::Capability)> = Vec::new();
    let mut add = |module: Option<&String>, cap: floor::Capability| {
        let key = module.cloned().unwrap_or_default();
        if !out.iter().any(|(m, c)| *m == key && *c == cap) {
            out.push((key, cap));
        }
    };

    // The two declarations no instance holds. A judged name is a builtin call,
    // so a walk for the name is the whole reading.
    let spells = |e: &vyrn_frontend::ast::Expr| -> Vec<floor::Capability> {
        let mut e = e.clone();
        let mut found: Vec<floor::Capability> = Vec::new();
        vyrn_frontend::project::walk_bare(&mut e, &mut |x| {
            if let vyrn_frontend::ast::Expr::Call { name, .. } = x {
                if let Some((_, cap)) = floor::JUDGED.iter().find(|(n, _)| n == name) {
                    found.push(*cap);
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

    // The effect a judged row is: one lookup, so the row and the atom cannot
    // drift apart.
    let rows: Vec<(Effect, floor::Capability)> = floor::JUDGED
        .iter()
        .filter_map(|(n, cap)| atom(n).map(|e| (e, *cap)))
        .collect();

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
    // Every frame, outermost first: `top[i]` is instance `i`'s own body, and a
    // lambda frame is keyed by the function it was written in and its line,
    // which is how RFC-0037 names a lambda source.
    let mut refs: Vec<&crate::core::Body> = Vec::new();
    let mut top: Vec<usize> = Vec::new();
    let mut lambda_frames: HashMap<(&str, usize), Vec<usize>> = HashMap::new();
    for (i, b) in bodies.iter().enumerate() {
        for f in b.frames() {
            if std::ptr::eq(f, b) {
                top.push(refs.len());
            } else if let Some(line) = f
                .name
                .rsplit("@lambda:")
                .next()
                .and_then(|l| l.parse::<usize>().ok())
            {
                lambda_frames
                    .entry((insts[i].func.name.as_str(), line))
                    .or_default()
                    .push(refs.len());
            }
            refs.push(f);
        }
    }
    let mut by_name: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, inst) in insts.iter().enumerate() {
        by_name
            .entry(inst.func.name.as_str())
            .or_default()
            .push(top[i]);
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
    let decls = vyrn_frontend::types::decl_map(program);
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
        Callee::Pure
    };
    let stored = vyrn_frontend::checker::stored_fn_effects(program);
    let mut through = |ty: &Type| -> Callee {
        let ty = &vyrn_frontend::types::resolve(ty, &decls);
        if !matches!(ty, Type::Fn(..)) {
            return Callee::Unknown;
        }
        let mut idx: Vec<usize> = Vec::new();
        for src in &stored.sources {
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
            Callee::Unknown
        } else {
            Callee::Bodies(idx)
        }
    };
    let judged = judge(&refs, &mut resolve, &mut through);
    for (i, inst) in insts.iter().enumerate() {
        // The generation context, which is the table's `gen` column becoming a
        // check (RFC-0125 §3 M6, fifth slice). A `gen fn` body runs at
        // GENERATION time against the compiler's filesystem and is never
        // compiled into the artifact, so what it reaches is no capability of
        // the artifact — the same rule `floor::carried` states by skipping a
        // `gen fn`, and the reason 216 corpus bodies are `gen-body` and not a
        // disagreement (finding 9). The fence decides what a generator may do;
        // the floor decides what a target may do; this is the line between.
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
    out
}
