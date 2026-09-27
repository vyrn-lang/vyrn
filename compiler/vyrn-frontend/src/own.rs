//! Whole-program ownership facts the emitters read: the release vocabulary
//! ([`Release`], [`Exit`], [`DropKind`]), the `vyrn why --memory` rows, and the
//! slots through which `vyrn-lower`, which sits above this crate, installs the
//! placer and its judgments. [`Owned`] answers what owns; the core's placer
//! decides what is released where. Nodes are keyed by address, so a reader must
//! walk the same borrowed AST the analysis walked (see [`ReleasePlan`] for
//! clones).

use std::collections::HashMap;

use crate::ast::*;
use crate::declared::Owned;

/// Which exit runs a release step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Exit {
    /// The fall-through end of a block.
    Block,
    /// The temporary a `match`, `if let` or `for in` owns, keyed by the
    /// construct. No row exists where an arm hands the payload out.
    Scrutinee,
    /// Every frame the innermost loop's body opened.
    Break,
    /// The same frames as [`Exit::Break`].
    Continue,
    /// Every frame the function has open.
    Return,
    /// A propagating `?`, which releases what a `return` does.
    Try,
}

/// One placed release: a binding, how it is reclaimed, and the exit that runs
/// it, in run order. The kind is unsubstituted; a reader of a generic instance
/// substitutes it.
#[derive(Debug, Clone)]
pub struct Release {
    /// The node address the exit is at: a `Block`, the owning `match` /
    /// `if let` / `for in`, a `Stmt::Break` / `Continue` / `Return`, or an
    /// `Expr::Try`.
    pub site: usize,
    /// The owning node's address: a `Stmt::Let`, or the construct for its
    /// temporary.
    pub binding: usize,
    pub name: String,
    pub kind: DropKind,
    pub exit: Exit,
    pub line: u32,
    /// The holes this row walks around at this exit, which may differ from the
    /// binding's set on another path. `None` means the binding's own set.
    pub holes: Option<Vec<String>>,
}

/// How a binding is reclaimed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DropKind {
    FreeStr,
    FreeArr,
    /// Frees the `data` buffer, which is null while inline, so the site is the
    /// same whether it spilled or not.
    FreeSmallArr,
    /// Frees both backing buffers (keys and values).
    FreeMap,
    /// One call to `@__vyrn_stream_close`, which branches on the runtime
    /// variant tag, so every drop site stays straight-line.
    CloseStream,
    /// An aggregate holding heap in its places (a field, a slot, a payload, a
    /// capture block). The walk is the type, as for `copy`, and only the live
    /// variant of a payload is released.
    Deep(Type),
    /// A call to the flattened `release` a type declared by `impl Owned for T`,
    /// with the receiver type [`Owned::release_kind`] was asked about,
    /// unsubstituted. A generic `release` needs the type to pick its instance.
    Release(String, Type),
}

impl DropKind {
    /// Returns how this kind reclaims, in the words `vyrn why --memory` and the
    /// LSP hover both print.
    pub fn words(&self) -> String {
        match self {
            DropKind::FreeStr => "freeing the String buffer".into(),
            DropKind::FreeArr => "freeing the array buffer".into(),
            DropKind::FreeSmallArr => "freeing the spilled buffer, if it spilled".into(),
            DropKind::FreeMap => "freeing both map buffers".into(),
            DropKind::CloseStream => "closing the stream".into(),
            DropKind::Deep(ty) => format!("releasing what the {ty} holds"),
            DropKind::Release(f, _) => format!("calling `{f}`"),
        }
    }
}

/// Which row gives a type its must-use obligation. The rows differ in how the
/// obligation is discharged, so a diagnostic offers a different menu for each.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Linear {
    /// A `Stream<T>`: consumed with `for ... in`, returned, or `close(s)`d.
    /// `drop s` reclaims nothing: [`Owned::release_kind`] answers `None` for it.
    Stream,
    /// An `impl MustUse for T` row: handed on by name, or released with
    /// `drop t`. Carries the type key that declared the row, which differs from
    /// the type asked about for `Array<Txn>`.
    Declared(String),
}

/// One `let` binding and what happens to its value, for `vyrn why --memory`
/// and the editor's memory hints. The core's placer writes it.
#[derive(Clone, Debug)]
pub struct MemoryRow {
    pub name: String,
    /// 1-based line of the `let`.
    pub line: usize,
    /// What happens to the value, in one line.
    pub text: String,
    /// The line of the move or `drop` that ends the value; `None` when it lives
    /// to block exit.
    pub last_use: Option<usize>,
    /// What took the value; `Some` exactly when it moved.
    pub moved_into: Option<String>,
    pub bucket: Bucket,
}

/// The counters `vyrn why --memory` sums, and the grouping its leak table
/// prints.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Bucket {
    Reclaimed,
    Moved,
    Dropped,
    Static,
    Discharged,
    /// `reason` is the row without lines and names, so a corpus groups; `heap`
    /// is false for a scalar, which gets no editor hint.
    Leaked {
        reason: &'static str,
        heap: bool,
    },
}

/// Maps a cloned node back to the node the core keyed. A user-container `for`
/// and a `place at` rewrite clone the statements they expand, so the core's
/// answers sit on nodes the emission never walks; [`ReleasePlan::key_of`]
/// resolves them.
#[derive(Clone, Default)]
pub struct ReleasePlan {
    /// Clone address to original address; chains for a clone of a clone.
    alias: std::cell::RefCell<HashMap<usize, usize>>,
}

impl ReleasePlan {
    /// Registers clone-to-original pairs for a clone that lives as long as the
    /// compile.
    pub fn alias_clones(&self, pairs: &[(usize, usize)]) {
        self.alias.borrow_mut().extend(pairs.iter().copied());
    }

    fn resolve(&self, mut at: usize) -> usize {
        let alias = self.alias.borrow();
        // Bounded by clone nesting depth; the cap guards against a cycle a
        // defect in the pair builder could create.
        for _ in 0..64 {
            match alias.get(&at) {
                Some(next) => at = *next,
                None => break,
            }
        }
        at
    }

    /// Returns the node the core keyed for `at`: itself, or the original a
    /// clone copies.
    pub fn key_of(&self, at: usize) -> usize {
        self.resolve(at)
    }
}

#[derive(Clone, Default)]
pub struct Ownership {
    pub plan: ReleasePlan,
    /// Per function, every `let` in source order and its fate, written by the
    /// placer. Empty without a placer and for a body the core does not lower.
    pub memory: HashMap<String, Vec<MemoryRow>>,
    /// The table this analysis decided with, so an explicit `drop x` asks the
    /// same question as the automatic path.
    pub proto: Owned,
    /// Per function, every release step in run order, grouped by
    /// [`Release::site`].
    pub releases: HashMap<String, Vec<Release>>,
    /// See [`crate::movecheck::Facts::fnval_clear`]. The core asks it at a call
    /// through a fn value, where no capability row answers.
    pub fnval_clear: std::collections::HashSet<String>,
    /// See [`crate::declared::arg_caps`]. It reads only declarations, so one
    /// table serves every body.
    pub arg_caps: HashMap<String, Vec<Capability>>,
}

/// Holds one ownership analysis per command, adopting the load's through
/// [`hand_on`], so the lowering does not recompute it.
///
/// The guard borrows its program, so no other `Program` can take that address
/// while it is held: a hit is the same program. Any other program analysed
/// inside the guard (a generator's, during a load) misses and is not cached.
/// Only the CLI opens one, beside [`crate::project::Memo`].
pub struct Memo<'a> {
    program: std::marker::PhantomData<&'a Program>,
}

/// A program's identity from the load to the lowering. The `Program` moves in
/// between, but its `functions` buffer does not, so the buffer address plus the
/// two lengths a synthesis can change survive the move.
fn ident(program: &Program) -> (usize, usize, usize) {
    (
        program.functions.as_ptr() as usize,
        program.functions.len(),
        program.type_decls.len(),
    )
}

thread_local! {
    /// The load's analysis and the [`ident`] of its program.
    static LOADED: std::cell::RefCell<Option<((usize, usize, usize), Ownership)>> =
        const { std::cell::RefCell::new(None) };
}

/// Hands the load's analysis to the [`Memo`] the command opens next.
///
/// Only inside a compile scope ([`crate::project::memo_open`]): there a
/// projection site keeps one expansion, so the load's nodes are the ones the
/// command lowers. The editor opens none, because a reused `functions` buffer
/// would make [`ident`] match two different texts.
pub fn hand_on(program: &Program, ownership: &Ownership) {
    if !crate::project::memo_open() {
        return;
    }
    LOADED.with(|l| *l.borrow_mut() = Some((ident(program), ownership.clone())));
}

/// Drops what the load handed on. Call it after rewriting the program in place
/// (`vyrn serve` renames calls), which [`ident`] cannot see.
pub fn forget_loaded() {
    LOADED.with(|l| *l.borrow_mut() = None);
}

thread_local! {
    /// The program this memo answers for, or 0. An address, never dereferenced.
    static MEMO_FOR: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static MEMO: std::cell::RefCell<Option<Ownership>> = const {
        std::cell::RefCell::new(None)
    };
}

impl<'a> Memo<'a> {
    /// Holds one analysis of `program` until the guard drops, adopting the
    /// load's when it was made for this program. The CLI must open
    /// [`crate::project::Memo`] before the load, or a projection site is
    /// inlined twice under different tags and the adopted rows key nothing.
    pub fn open(program: &'a Program) -> Memo<'a> {
        MEMO_FOR.with(|p| p.set(program as *const Program as usize));
        let adopted = LOADED
            .with(|l| l.borrow_mut().take())
            .filter(|_| crate::project::memo_open())
            .filter(|(id, _)| *id == ident(program))
            .map(|(_, o)| o);
        MEMO.with(|m| *m.borrow_mut() = adopted);
        // The lowering reads the checker's record (`checker::recorded`)
        // instead of checking the program again.
        crate::checker::hold_open(program);
        Memo {
            program: std::marker::PhantomData,
        }
    }
}

impl Drop for Memo<'_> {
    fn drop(&mut self) {
        MEMO_FOR.with(|p| p.set(0));
        MEMO.with(|m| *m.borrow_mut() = None);
        crate::checker::hold_close();
    }
}

/// Analyses ownership across a whole program, served from the open [`Memo`]
/// when it holds one for `program`.
pub fn analyze(program: &Program) -> Ownership {
    let key = program as *const Program as usize;
    let memoed = MEMO_FOR.with(|p| p.get()) == key;
    if memoed {
        if let Some(o) = MEMO.with(|m| m.borrow().clone()) {
            return o;
        }
    }
    let ownership = analyze_now(program);
    if memoed {
        MEMO.with(|m| *m.borrow_mut() = Some(ownership.clone()));
    }
    ownership
}

fn analyze_now(program: &Program) -> Ownership {
    let _p = crate::prof::phase("own: analyze_now");
    let ps = crate::prof::phase("own: Owned::new");
    let proto = Owned::new(program);
    drop(ps);
    let fs = crate::prof::phase("own: movecheck::facts");
    let facts = crate::movecheck::facts(program);
    drop(fs);
    let plan = ReleasePlan::default();
    let mut ownership = Ownership {
        plan,
        memory: HashMap::new(),
        proto,
        // Only the placer writes release rows.
        releases: HashMap::new(),
        fnval_clear: facts.fnval_clear.clone(),
        arg_caps: crate::declared::arg_caps(program),
    };
    // The placer runs the lowering, which runs this analysis, so it is not
    // re-entered.
    if let Some(place) = PLACER.get() {
        if !PLACING.with(|p| p.get()) {
            PLACING.with(|p| p.set(true));
            place(program, &mut ownership);
            PLACING.with(|p| p.set(false));
        }
    }
    ownership
}

/// The plan's key for a `for` variable, which has no `let` node: the address
/// of its name's heap buffer. Not the `String`'s own address: it sits at offset
/// 0 of `Stmt::ForIn`, so it equals the statement's key, the container row's.
pub fn for_var_key(var: &str) -> usize {
    var.as_ptr() as usize
}

/// The plan's key for a pattern binder, which has no `let` node: the address of
/// its name's heap buffer, as in [`for_var_key`]. The core and the emitters take
/// it off the same pattern node.
pub fn binder_key(name: &str) -> usize {
    name.as_ptr() as usize
}

/// A pass that adds release rows to a finished analysis: the core's placer,
/// which lives in `vyrn-lower`, above this crate.
pub type Placer = fn(&Program, &mut Ownership);

static PLACER: std::sync::OnceLock<Placer> = std::sync::OnceLock::new();

thread_local! {
    static PLACING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Installs the placer. The first installation wins.
pub fn install_placer(f: Placer) {
    let _ = PLACER.set(f);
}

/// Whether anything in this process lowers a program. The checker keeps its
/// record only when this is true, because the lowering is its one reader.
pub fn placer_installed() -> bool {
    PLACER.get().is_some()
}

/// Drains the kernel's refusals about the program the placer just judged. A
/// generator's own load drains its refusals, so what is left is this
/// program's.
pub type Refusals = fn() -> Vec<crate::diagnostics::Diagnostic>;

static REFUSALS: std::sync::OnceLock<Refusals> = std::sync::OnceLock::new();

/// Installs the kernel's refusal drain. The first installation wins.
pub fn install_refusals(f: Refusals) {
    let _ = REFUSALS.set(f);
}

/// Returns what the kernel refuses about the program just analysed; empty when
/// nothing is installed or under `VYRN_NO_KERNEL=1`.
pub fn kernel_refusals() -> Vec<crate::diagnostics::Diagnostic> {
    REFUSALS.get().map(|f| f()).unwrap_or_default()
}

/// The must-use judgment (`vyrn_lower::typed::obligation`). Unlike the drains,
/// it is asked of a program and holds no state between calls.
pub type MustUse = fn(&Program) -> Vec<crate::diagnostics::Diagnostic>;

static MUST_USE: std::sync::OnceLock<MustUse> = std::sync::OnceLock::new();

/// Installs the must-use judgment. The first installation wins.
pub fn install_must_use(f: MustUse) {
    let _ = MUST_USE.set(f);
}

/// Returns what the must-use judgment refuses about `program`; empty when
/// nothing is installed.
pub fn must_use_refusals(program: &Program) -> Vec<crate::diagnostics::Diagnostic> {
    MUST_USE.get().map(|f| f(program)).unwrap_or_default()
}

/// Drains the typed judgment's refusals, like [`Refusals`].
pub type Typed = fn() -> Vec<crate::diagnostics::Diagnostic>;

static TYPED: std::sync::OnceLock<Typed> = std::sync::OnceLock::new();

/// Installs the typed judgment's drain. The first installation wins.
pub fn install_typed(f: Typed) {
    let _ = TYPED.set(f);
}

/// Returns what the typed judgment refused about the program just analysed.
///
/// # Panics
///
/// If a placer is installed and this slot is not: the checker does not state
/// these rules, so an empty slot would be a silent acceptance.
pub fn typed_refusals() -> Vec<crate::diagnostics::Diagnostic> {
    match TYPED.get() {
        Some(f) => f(),
        None if placer_installed() => {
            panic!("the placer is installed and the typed judgment is not")
        }
        None => Vec::new(),
    }
}

/// Groups release steps by `(exit, site)`, each binding with its row's
/// [`Release::holes`], in run order.
pub fn placed(steps: &[Release]) -> HashMap<(Exit, usize), Vec<(usize, Option<Vec<String>>)>> {
    let mut out: HashMap<(Exit, usize), Vec<(usize, Option<Vec<String>>)>> = HashMap::new();
    for r in steps {
        out.entry((r.exit, r.site))
            .or_default()
            .push((r.binding, r.holes.clone()));
    }
    out
}
