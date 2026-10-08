//! The World: one program's analysis as a value. [`analyze`] makes it, and
//! the pipeline's judgments and every emitter read it by reference. It holds
//! no `Rc` and no cell, so it is `Send + Sync`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use vyrn_frontend::ast::{FnId, Function, Key, Program, Type};
use vyrn_frontend::checker;
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
    /// The read relation over the function table, which [`Reads::replace`]
    /// writes.
    pub(crate) reads: Reads,
    /// The core's bodies, which [`World::body_of`] serves. `None` under a
    /// name two bodies share. Empty when `facts` is `None`.
    pub(crate) bodies: HashMap<FnId, Option<Stated>>,
    /// The kernel's hard refusals in the order the placer met them: a double
    /// free, a use after release, a join whose edges disagree, and a rule the
    /// core states about a construct it does lower.
    pub(crate) refusals: Vec<Refusal>,
    /// What the typed judgment refused, as `vyrn check` words it.
    pub(crate) typed: Vec<Diagnostic>,
    /// The effect judgment's set for each instance the placer judged, but a
    /// `gen fn`'s, with the instance's module: what [`crate::effects::reaches`]
    /// reads.
    pub(crate) reached: Vec<(Option<String>, vyrn_frontend::effects::Effects)>,
    /// The functions declared outside the root file whose effect set holds `alloc`, as the
    /// placer judged them, a body the judgment memo served included, with their file: what
    /// [`World::allocating_file`] answers.
    pub(crate) allocating: HashMap<FnId, String>,
    /// Whether a placed release named an instance the first lowering lacked.
    /// `reached` holds no such instance, so [`crate::effects::reaches`] judges
    /// the program as placed instead.
    pub(crate) late: bool,
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
/// each instance, then each lambda frame and the module state in the order
/// the placer's serial merge meets it ([`Fns::number`]). Deterministic, so
/// one program numbers alike on every run; no output is read in id order.
/// A worker reads the table and adds no row.
#[derive(Default)]
pub struct Fns {
    rows: Vec<FnRow>,
    /// The first row under each name. A non-generic instance and its frame
    /// are the function's own row.
    ids: HashMap<String, FnId>,
    /// The projections' rows ([`crate::Lowered::places`]). Two impls may
    /// declare a projection of one name, so a projection's body carries its
    /// row and no reader asks for one by name.
    places: Vec<FnId>,
}

impl Fns {
    /// The table of `lowered`: a row per [`crate::Lowered::source`] name,
    /// even where a projection shares a function's name, then a row per
    /// instance ([`Fns::instance`]).
    pub(crate) fn lowered(lowered: &crate::Lowered) -> Fns {
        let mut fns = Fns::default();
        for name in &lowered.source {
            fns.push(name.clone(), None);
        }
        for inst in &lowered.instances {
            fns.instance(inst);
        }
        fns.places = lowered.places.iter().map(|p| p.id).collect();
        fns
    }

    /// The first row named `name`.
    pub(crate) fn id(&self, name: &str) -> Option<FnId> {
        let id = self.ids.get(name).copied();
        debug_assert!(
            id.is_none_or(|id| !self.places.contains(&id)),
            "`{name}` asked a projection's row by name, which another impl's projection may share"
        );
        id
    }

    /// The row named `name`, added when there is none.
    pub(crate) fn add(&mut self, name: &str) -> FnId {
        match self.id(name) {
            Some(id) => id,
            None => self.push(name.to_string(), None),
        }
    }

    /// The row of `inst`, added when there is none.
    pub(crate) fn instance(&mut self, inst: &crate::Instance) -> FnId {
        match self.instance_id(inst) {
            Some(id) => id,
            None => self.push(
                inst.spelling(),
                Some((inst.func_id, inst.type_args.clone())),
            ),
        }
    }

    /// The row of `inst`: its function's own row when it has no type
    /// arguments.
    pub(crate) fn instance_id(&self, inst: &crate::Instance) -> Option<FnId> {
        if inst.type_args.is_empty() {
            return Some(inst.func_id);
        }
        self.id(&inst.spelling())
    }

    /// Gives every frame of `top` without a row the row named as the frame
    /// ([`Fns::add`]), and returns each frame's row in [`Body::frames`]
    /// order. The serial merge calls it on every body a worker built.
    pub(crate) fn number(&mut self, top: &mut Body) -> Vec<FnId> {
        let mut ids = Vec::new();
        top.each_frame_mut(&mut |b| {
            ids.push(*b.id.get_or_insert_with(|| self.add(&b.name)));
        });
        // A lambda frame is spelled by its outer body, line and column. Two
        // projections inlined into one body could each bring a lambda at one
        // line and column from two files; the frames would then share a row,
        // and with it a body and its releases. No program reaches this: the
        // core states no body for a caller that inlines a lambda.
        debug_assert!(
            (1..ids.len()).all(|i| !ids[..i].contains(&ids[i])),
            "two frames of `{}` share a row",
            top.name
        );
        ids
    }

    fn push(&mut self, name: String, generic: Option<(FnId, Vec<Type>)>) -> FnId {
        let id = FnId::nth(self.rows.len());
        self.ids.entry(name.clone()).or_insert(id);
        self.rows.push(FnRow { name, generic });
        id
    }
}

/// The call relation between source bodies ([`Program::source_id`]). A
/// caller is the body a frame belongs to: every instance of a generic and
/// every lambda frame count under the function's own row, a module-state
/// initializer and a `where` predicate under their own. A callee is a
/// [`Callee::Fn`] row or a declared release a name runs. A call through a
/// value, an undispatched method and a projection resolve to no function, so
/// they are no edge. A body the judgment memo serves is not built, so it has
/// no edges.
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

/// The read relation between source bodies and the name lookups their text
/// makes ([`vyrn_frontend::checker::Recorded::reads`]): a declaration found,
/// or a name missed in a scope. A reader is a [`Program::source_id`] row.
#[derive(Default)]
pub(crate) struct Reads {
    /// By reader: its keys in the order read, each once.
    keys: Vec<Vec<Key>>,
    /// By key: its readers in id order, each once. A key no function reads
    /// has no entry.
    readers: HashMap<Key, Vec<FnId>>,
}

impl Reads {
    /// Replaces the keys of every reader in `rows` and the reverse entries
    /// with them, in one batch. A reader absent from `rows` keeps its keys.
    /// Each list in `rows` holds a key once.
    pub(crate) fn replace(&mut self, rows: HashMap<FnId, Vec<Key>>) {
        let n = (rows.keys().map(|f| f.index() + 1).max().unwrap_or(0)).max(self.keys.len());
        self.keys.resize_with(n, Vec::new);
        let mut replaced = vec![false; n];
        let mut touched: HashSet<Key> = HashSet::new();
        for f in rows.keys() {
            replaced[f.index()] = true;
            touched.extend(std::mem::take(&mut self.keys[f.index()]));
        }
        for k in &touched {
            if let Some(rs) = self.readers.get_mut(k) {
                rs.retain(|r| !replaced[r.index()]);
            }
        }
        for (f, ks) in rows {
            for k in &ks {
                self.readers.entry(k.clone()).or_default().push(f);
            }
            touched.extend(ks.iter().cloned());
            self.keys[f.index()] = ks;
        }
        for k in touched {
            let Some(rs) = self.readers.get_mut(&k) else {
                continue;
            };
            if rs.is_empty() {
                self.readers.remove(&k);
            } else {
                rs.sort_unstable_by_key(|r| r.index());
            }
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
/// none of these; it walks its own copy of a body with a group of stores.
pub(crate) struct Stated {
    pub(crate) body: Body,
    pub(crate) decided: OnceLock<Body>,
}

const _: () = {
    const fn crosses_threads<T: Send + Sync>() {}
    crosses_threads::<World>()
};

impl World {
    /// The World of `program`'s `ownership`, with the read relation of its
    /// checker's record.
    pub(crate) fn new(program: &Program, ownership: Ownership) -> World {
        let mut rows: HashMap<FnId, Vec<Key>> = HashMap::new();
        for (body, k) in &ownership.record.reads {
            let ks = rows.entry(program.source_id(*body)).or_default();
            if !ks.contains(k) {
                ks.push(k.clone());
            }
        }
        let mut world = World {
            ownership,
            ..World::default()
        };
        world.reads.replace(rows);
        world
    }

    /// The id of the function emitted under `name`: [`crate::spell`] of the
    /// instance (`max<Int64>`, `main@lambda:26:13`, `test@1`, or the empty
    /// name for module state), or a function's own name. A reader outside
    /// the World looks a name up once, here.
    pub fn fn_id(&self, name: &str) -> Option<FnId> {
        self.fns.id(name)
    }

    /// The function table's rows, the row of `id` at [`FnId::index`].
    pub fn fn_rows(&self) -> &[FnRow] {
        &self.fns.rows
    }

    /// The core's body for the function emitted under `name` ([`World::fn_id`]).
    /// `None` for a gap and for a name two bodies share; a reader then walks
    /// the source.
    pub fn body_of(&self, name: &str) -> Option<&Body> {
        self.body_at(self.fn_id(name)?)
    }

    /// [`World::body_of`] for the function table's row `id`.
    pub fn body_at(&self, id: FnId) -> Option<&Body> {
        let s = self.bodies.get(&id)?.as_ref()?;
        if !crate::core::decides() {
            return Some(&s.body);
        }
        Some(s.decided.get_or_init(|| {
            let mut body = s.body.clone();
            crate::elide::decide(&mut body, self.ownership.proto.types());
            body
        }))
    }

    /// The file `f` is declared in, when that is not the root file and a call to `f` may
    /// allocate: its effect set holds `alloc`.
    pub fn allocating_file(&self, f: FnId) -> Option<&str> {
        self.allocating.get(&f).map(String::as_str)
    }

    /// The functions `f`'s bodies call ([`Calls`]), in source order.
    pub fn callees(&self, f: FnId) -> &[FnId] {
        self.calls.callees.get(f.index()).map_or(&[], Vec::as_slice)
    }

    /// The functions whose bodies call `f` ([`Calls`]).
    pub fn callers(&self, f: FnId) -> &[FnId] {
        self.calls.callers.get(f.index()).map_or(&[], Vec::as_slice)
    }

    /// The functions whose text read `key` ([`Reads`]).
    pub fn readers(&self, key: &Key) -> &[FnId] {
        self.reads.readers.get(key).map_or(&[], Vec::as_slice)
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
    /// A must-use row ([`crate::rules::owed`]) is one per binding: file, line
    /// and the binding it is about, the first instance's words.
    pub fn refusal_diagnostics(&self) -> Vec<Diagnostic> {
        if !crate::core::refuses() {
            return Vec::new();
        }
        let mut seen = HashSet::new();
        let mut owed = HashSet::new();
        let mut body = "";
        let mut nth: HashMap<(Option<String>, usize, String), usize> = HashMap::new();
        (self.refusals.iter())
            .filter(|r| {
                if r.body != body {
                    body = &r.body;
                    nth.clear();
                }
                let d = &r.diagnostic;
                if let Some(binding) = crate::rules::owed(d) {
                    return owed.insert((d.file.clone(), d.line, binding.to_string()));
                }
                let key = (d.file.clone(), d.line, d.message.clone());
                let n = nth.entry(key.clone()).or_default();
                *n += 1;
                seen.insert((key, *n))
            })
            .map(|r| r.diagnostic.clone())
            .collect()
    }

    /// The must-use rows of [`World::refusal_diagnostics`] in source order,
    /// the only kernel refusals a generator's program prints.
    pub fn owed_diagnostics(&self) -> Vec<Diagnostic> {
        let mut owed = self.refusal_diagnostics();
        owed.retain(|d| crate::rules::owed(d).is_some());
        vyrn_frontend::movecheck::in_source_order(&mut owed);
        owed
    }

    /// Asserts the invariants the World holds when [`analyze`] returns it.
    ///
    /// # Panics
    ///
    /// If a name's id is not its row's, if a body is served under a name
    /// other than its own, if bodies exist without facts, if a refusal is
    /// not an error, or if the callers do not invert the callees or the
    /// readers the reads.
    pub fn check(&self) {
        let rows = self.fns.rows.len();
        for (f, ks) in self.reads.keys.iter().enumerate() {
            for k in ks {
                assert!(f < rows, "a read names no row");
                let once = self.readers(k).iter().filter(|r| r.index() == f).count();
                assert_eq!(once, 1, "a key lists its reader other than once");
            }
        }
        let reads = self.reads.keys.iter().map(Vec::len).sum::<usize>();
        let readers = self.reads.readers.values().map(Vec::len).sum::<usize>();
        assert_eq!(reads, readers, "a reader entry has no read");
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
            assert_eq!(
                &self.fn_rows()[id.index()].name,
                name,
                "a name's id is another row"
            );
        }
        for (id, s) in &self.bodies {
            if let Some(s) = s {
                assert_eq!(
                    s.body.id,
                    Some(*id),
                    "a core body is served under another row"
                );
                assert_eq!(
                    s.body.name,
                    self.fn_rows()[id.index()].name,
                    "a core body is served under another name"
                );
            }
        }
        // An analysis that armed the judgment memo folds no facts, but builds the root file's own
        // bodies for the editor ([`crate::insight::fn_costs`]).
        assert!(
            self.facts.is_some() || (self.bodies.values().flatten()).all(|s| s.body.file.is_none()),
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

/// Analyses ownership across `program`, places the releases the plan did not
/// place ([`crate::core::augment`]), and returns the World every consumer
/// reads. It checks `program` again for the record; a host holding the
/// check's World passes it on instead.
pub fn analyze(program: &Program) -> Arc<World> {
    analyzed(program, Arc::new(checker::record(program)), false)
}

/// [`analyze`] against `record`, the checker's record of `program`. `judging`
/// marks the analysis whose refusals [`crate::refusals`] reports, the only one
/// that may reuse a judgment or skip the emitter's facts.
pub(crate) fn analyzed(
    program: &Program,
    record: Arc<checker::Recorded>,
    judging: bool,
) -> Arc<World> {
    let mut world = World::new(program, own::analyze(program, record));
    crate::core::augment(program, &mut world, judging);
    #[cfg(debug_assertions)]
    world.check();
    Arc::new(world)
}
