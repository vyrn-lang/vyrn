//! The World: one program's analysis as a value. [`analyze`] makes it, and
//! the pipeline's judgments and every emitter read it by reference. It holds
//! no `Rc` and no cell, so it is `Send + Sync`.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use vyrn_frontend::ast::{FnId, Function, Program, Type};
use vyrn_frontend::core::{rows, Body, Callee, Facts, Rhs, St};
use vyrn_frontend::diagnostics::{Diagnostic, Severity};
use vyrn_frontend::movecheck::Refusal;
use vyrn_frontend::own::{self, Ownership};

#[derive(Default)]
pub struct World {
    /// The plan with the placer's release and memory rows, and the checker's
    /// record it was lowered against.
    pub ownership: Ownership,
    /// The core's answers per node. `None` for an analysis that feeds no
    /// emitter ([`vyrn_frontend::movecheck::emitting`]).
    pub facts: Option<Facts>,
    /// The function table: every function, instance and frame the analysis
    /// met, numbered as [`Fns`] says.
    pub(crate) fns: Fns,
    /// The call relation over the function table, which [`Calls::replace`]
    /// writes.
    pub(crate) calls: Calls,
    /// The core's bodies, which [`World::body_of`] serves. `None` under a
    /// name two bodies share. Empty when `facts` is `None`.
    pub(crate) bodies: HashMap<FnId, Option<Stated>>,
    /// The kernel's hard refusals in the order the placer met them: a double
    /// free, a use after release, a join whose edges disagree, and a rule the
    /// core states about a construct it does lower.
    pub(crate) refusals: Vec<Refusal>,
    /// What the typed judgment refused, as `vyrn check` words it.
    pub(crate) typed: Vec<Diagnostic>,
}

/// One row of the World's function table.
#[derive(Debug)]
pub struct FnRow {
    /// The name the function is emitted and looked up under ([`crate::spell`]).
    pub name: String,
    /// An instance of a generic: the generic's row and the type arguments.
    pub generic: Option<(FnId, Vec<Type>)>,
}

/// The function rows in [`FnId`] order: [`crate::Lowered::source`], then
/// each instance, lambda frame and the module state in the order the placer
/// meets it. Deterministic, so one program numbers alike on every run; no
/// output is read in id order.
#[derive(Default)]
pub(crate) struct Fns {
    rows: Vec<FnRow>,
    /// The first row under each name. A non-generic instance and its frame
    /// are the function's own row.
    ids: HashMap<String, FnId>,
}

impl Fns {
    /// The table of [`crate::Lowered::source`], one row per name even where a
    /// projection shares a function's name.
    pub(crate) fn source(names: &[String]) -> Fns {
        let mut fns = Fns::default();
        for name in names {
            fns.push(name.clone(), None);
        }
        fns
    }

    /// The row named `name`, added with `generic` when there is none.
    pub(crate) fn add(&mut self, name: &str, generic: Option<(FnId, Vec<Type>)>) -> FnId {
        match self.ids.get(name) {
            Some(&id) => id,
            None => self.push(name.to_string(), generic),
        }
    }

    /// The row of `inst`: its function's own row when it has no type
    /// arguments.
    pub(crate) fn instance(&mut self, inst: &crate::Instance) -> FnId {
        if inst.type_args.is_empty() {
            return inst.func_id;
        }
        let generic = Some((inst.func_id, inst.type_args.clone()));
        self.add(&inst.spelling(), generic)
    }

    fn push(&mut self, name: String, generic: Option<(FnId, Vec<Type>)>) -> FnId {
        let id = FnId::nth(self.rows.len());
        self.ids.entry(name.clone()).or_insert(id);
        self.rows.push(FnRow { name, generic });
        id
    }
}

/// The call relation between source functions. A caller is the function a
/// body belongs to: every instance of a generic and every lambda frame count
/// under the function's own row, the module-state initializers under the
/// empty name's. A callee is a [`Callee::Fn`] row or a declared release a
/// name runs. A call through a value, an undispatched method and a
/// projection resolve to no function, so they are no edge. A `where`
/// predicate has no row, and a body the judgment memo serves is not built,
/// so neither has edges.
#[derive(Default)]
pub(crate) struct Calls {
    /// By caller: its callees in source order, each once.
    callees: Vec<Vec<FnId>>,
    /// By callee: its callers in id order, each once.
    callers: Vec<Vec<FnId>>,
}

impl Calls {
    /// Replaces the callees of every caller in `rows` and the reverse
    /// entries with them, in one batch. A caller absent from `rows` keeps its
    /// edges. Each list in `rows` holds a callee once.
    pub(crate) fn replace(&mut self, rows: HashMap<FnId, Vec<FnId>>) {
        let ids = rows.keys().chain(rows.values().flatten());
        let n = (ids.map(|f| f.index() + 1).max().unwrap_or(0)).max(self.callees.len());
        self.callees.resize_with(n, Vec::new);
        self.callers.resize_with(n, Vec::new);
        let mut replaced = vec![false; n];
        let mut touched = vec![false; n];
        for f in rows.keys() {
            replaced[f.index()] = true;
            for g in &self.callees[f.index()] {
                touched[g.index()] = true;
            }
        }
        for (g, _) in touched.iter().enumerate().filter(|(_, t)| **t) {
            self.callers[g].retain(|c| !replaced[c.index()]);
        }
        for (f, cs) in rows {
            for g in &cs {
                touched[g.index()] = true;
                self.callers[g.index()].push(f);
            }
            self.callees[f.index()] = cs;
        }
        for (g, _) in touched.iter().enumerate().filter(|(_, t)| **t) {
            self.callers[g].sort_unstable_by_key(|c| c.index());
        }
    }
}

/// Appends to `out` each function a frame of `top` calls, in source order,
/// and each declared release a name of it runs, skipping one `out` holds.
/// `fns` resolves a release's name ([`crate::by_name`]).
pub(crate) fn add_callees(top: &Body, fns: &HashMap<&str, (FnId, &Function)>, out: &mut Vec<FnId>) {
    let mut add = |g: FnId| {
        if !out.contains(&g) {
            out.push(g);
        }
    };
    for b in top.frames() {
        for (s, _) in rows(&b.stmts) {
            if let St::Let(
                _,
                Rhs::Call {
                    kind: Callee::Fn(g),
                    ..
                },
            )
            | St::Do {
                rhs:
                    Rhs::Call {
                        kind: Callee::Fn(g),
                        ..
                    },
                ..
            } = s
            {
                add(*g);
            }
        }
        let runs = (b.names.iter()).flat_map(|i| &i.runs);
        runs.filter_map(|r| fns.get(r.as_str()))
            .for_each(|(g, _)| add(*g));
    }
}

/// One body with its check rows stated, and the same body decided
/// (`elide::decide`) when an emitter first reads it, so `vyrn check` decides
/// none.
pub(crate) struct Stated {
    pub(crate) body: Body,
    pub(crate) decided: OnceLock<Body>,
}

const _: () = {
    const fn crosses_threads<T: Send + Sync>() {}
    crosses_threads::<World>()
};

impl World {
    pub(crate) fn new(ownership: Ownership) -> World {
        World {
            ownership,
            ..World::default()
        }
    }

    /// The id of the function emitted under `name`: [`crate::spell`] of the
    /// instance (`max<Int64>`, `main@lambda:26:13`, `test@1`, or the empty
    /// name for module state), or a function's own name. A reader outside
    /// the World looks a name up once, here.
    pub fn fn_id(&self, name: &str) -> Option<FnId> {
        self.fns.ids.get(name).copied()
    }

    /// The row of `id`.
    ///
    /// # Panics
    ///
    /// If `id` is not this World's.
    pub fn fn_row(&self, id: FnId) -> &FnRow {
        &self.fns.rows[id.index()]
    }

    /// The core's body for the function emitted under `name` ([`World::fn_id`]).
    /// `None` for a gap and for a name two bodies share; a reader then walks
    /// the source.
    pub fn body_of(&self, name: &str) -> Option<&Body> {
        let s = self.bodies.get(&self.fn_id(name)?)?.as_ref()?;
        if !crate::core::decides() {
            return Some(&s.body);
        }
        Some(s.decided.get_or_init(|| {
            let mut body = s.body.clone();
            crate::elide::decide(&mut body, self.ownership.proto.types());
            body
        }))
    }

    /// The functions `f`'s bodies call ([`Calls`]), in source order.
    pub fn callees(&self, f: FnId) -> &[FnId] {
        self.calls.callees.get(f.index()).map_or(&[], Vec::as_slice)
    }

    /// The functions whose bodies call `f` ([`Calls`]).
    pub fn callers(&self, f: FnId) -> &[FnId] {
        self.calls.callers.get(f.index()).map_or(&[], Vec::as_slice)
    }

    /// The typed judgment's refusals.
    pub fn typed_diagnostics(&self) -> &[Diagnostic] {
        &self.typed
    }

    /// The kernel's refusals as `movecheck`-stage diagnostics, deduplicated,
    /// for the one list a file's refusals come out in ([`crate::refusals`]);
    /// the caller orders the list. Empty under `VYRN_NO_KERNEL=1`.
    ///
    /// Several instances of one generic body reach the same rule, and a reader
    /// is owed one sentence per mistake, so file, line and message are the
    /// identity, with the count of that sentence within one body's run:
    /// `out.push(s) out.push(s)` on one line is two mistakes. `file` is `None`
    /// for the root module, which tells `vyrn fix` the edit is its to make.
    pub fn refusal_diagnostics(&self) -> Vec<Diagnostic> {
        if !crate::core::refuses() {
            return Vec::new();
        }
        let mut seen = HashSet::new();
        let mut body = "";
        let mut nth: HashMap<(Option<String>, usize, String), usize> = HashMap::new();
        (self.refusals.iter())
            .filter(|r| {
                if r.body != body {
                    body = &r.body;
                    nth.clear();
                }
                let d = &r.diagnostic;
                let key = (d.file.clone(), d.line, d.message.clone());
                let n = nth.entry(key.clone()).or_default();
                *n += 1;
                seen.insert((key, *n))
            })
            .map(|r| r.diagnostic.clone())
            .collect()
    }

    /// Asserts the invariants the World holds when [`analyze`] returns it.
    ///
    /// # Panics
    ///
    /// If a name's id is not its row's, if a body is served under a name
    /// other than its own, if bodies exist without facts, if a refusal is
    /// not an error, or if the callers do not invert the callees.
    pub fn check(&self) {
        let rows = self.fns.rows.len();
        for (f, cs) in self.calls.callees.iter().enumerate() {
            for g in cs {
                assert!(f < rows && g.index() < rows, "a call edge names no row");
                assert_eq!(
                    self.calls.callers[g.index()]
                        .iter()
                        .filter(|c| c.index() == f)
                        .count(),
                    1,
                    "a callee lists its caller other than once"
                );
            }
        }
        let edges = |cs: &Vec<Vec<FnId>>| cs.iter().map(Vec::len).sum::<usize>();
        assert_eq!(
            edges(&self.calls.callees),
            edges(&self.calls.callers),
            "a caller entry has no call edge"
        );
        for (name, id) in &self.fns.ids {
            assert_eq!(&self.fn_row(*id).name, name, "a name's id is another row");
        }
        for (id, s) in &self.bodies {
            if let Some(s) = s {
                assert_eq!(
                    s.body.name,
                    self.fn_row(*id).name,
                    "a core body is served under another name"
                );
            }
        }
        assert!(
            self.facts.is_some() || self.bodies.is_empty(),
            "the core's bodies outlived its facts"
        );
        let refusals = self.refusals.iter().map(|r| &r.diagnostic);
        assert!(
            refusals
                .chain(&self.typed)
                .all(|d| d.severity == Severity::Error),
            "a refusal is not an error"
        );
    }
}

thread_local! {
    /// The load's World, handed on to the first analysis of its program
    /// inside the next [`own::Memo`], with the program's [`own::ident`].
    #[allow(clippy::type_complexity)]
    static LOADED: RefCell<Option<((usize, usize, usize), Arc<World>)>> =
        const { RefCell::new(None) };
    /// The World of the program the open [`own::Memo`] answers for, under that
    /// memo's generation ([`own::memo_scope`]).
    static MEMO: RefCell<Option<(u64, Arc<World>)>> = const { RefCell::new(None) };
}

/// Analyses ownership across `program`, places the releases the plan did not
/// place ([`crate::core::augment`]), and returns the World every consumer
/// reads. Served from the open [`own::Memo`] when it holds one for `program`.
pub fn analyze(program: &Program) -> Arc<World> {
    let open = own::memo_scope().map(|(_, g)| g);
    MEMO.with(|m| drop(m.borrow_mut().take_if(|(g, _)| Some(*g) != open)));
    let memo = (own::memo_scope())
        .filter(|(at, _)| *at == program as *const Program as usize)
        .map(|(_, g)| g);
    if memo.is_some() {
        let hit = MEMO.with(|m| m.borrow().as_ref().map(|(_, w)| w.clone()));
        if let Some(w) = hit.or_else(|| adopt(program)) {
            MEMO.with(|m| *m.borrow_mut() = memo.map(|g| (g, w.clone())));
            return w;
        }
    }
    let mut world = World::new(own::analyze(program));
    crate::core::augment(program, &mut world);
    #[cfg(debug_assertions)]
    world.check();
    let world = Arc::new(world);
    if let Some(g) = memo {
        MEMO.with(|m| *m.borrow_mut() = Some((g, world.clone())));
    }
    world
}

/// The load's World, when it was made for `program` in a compile scope.
fn adopt(program: &Program) -> Option<Arc<World>> {
    (LOADED.with(|l| l.borrow_mut().take()))
        .filter(|_| vyrn_frontend::project::memo_open())
        .filter(|(id, _)| *id == own::ident(program))
        .map(|(_, w)| w)
}

/// Hands the load's World and checker record to the [`own::Memo`] the command
/// opens next ([`own::hand_on`]).
pub fn hand_on(program: &Program, world: &Arc<World>) {
    if !vyrn_frontend::project::memo_open() {
        return;
    }
    own::hand_on(program);
    LOADED.with(|l| *l.borrow_mut() = Some((own::ident(program), world.clone())));
}

/// Drops what the load handed on. Call it after rewriting the program in place
/// (`vyrn serve` renames calls), which [`own::ident`] cannot see.
pub fn forget_loaded() {
    own::forget_loaded();
    LOADED.with(|l| *l.borrow_mut() = None);
}
