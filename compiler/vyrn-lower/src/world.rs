//! The World: one program's analysis as a value. [`analyze`] makes it, and
//! the pipeline's judgments and every emitter read it by reference. It holds
//! no `Rc` and no cell, so it is `Send + Sync`.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use vyrn_frontend::ast::Program;
use vyrn_frontend::core::{Body, Facts};
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
    /// The core's bodies by the name each is emitted under ([`crate::spell`]),
    /// which [`World::body_of`] serves. `None` under a name two bodies share.
    /// Empty when `facts` is `None`.
    pub(crate) bodies: HashMap<String, Option<Stated>>,
    /// The kernel's hard refusals in the order the placer met them: a double
    /// free, a use after release, a join whose edges disagree, and a rule the
    /// core states about a construct it does lower.
    pub(crate) refusals: Vec<Refusal>,
    /// What the typed judgment refused, as `vyrn check` words it.
    pub(crate) typed: Vec<Diagnostic>,
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

    /// The core's body for the function emitted under `name`: [`crate::spell`]
    /// of the instance (`max<Int64>`, `main@lambda:26:13`, `test@1`, or the
    /// empty name for module state). `None` for a gap and for a name two
    /// bodies share; a reader then walks the source.
    pub fn body_of(&self, name: &str) -> Option<&Body> {
        let s = self.bodies.get(name)?.as_ref()?;
        if !crate::core::decides() {
            return Some(&s.body);
        }
        Some(s.decided.get_or_init(|| {
            let mut body = s.body.clone();
            crate::elide::decide(&mut body, self.ownership.proto.types());
            body
        }))
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
    /// If a body is served under a name other than its own, if bodies exist
    /// without facts, or if a refusal is not an error.
    pub fn check(&self) {
        for (name, s) in &self.bodies {
            if let Some(s) = s {
                assert_eq!(
                    &s.body.name, name,
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
