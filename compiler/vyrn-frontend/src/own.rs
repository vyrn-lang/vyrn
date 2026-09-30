//! Whole-program ownership facts the emitters read: the release vocabulary
//! ([`Release`], [`Exit`], [`DropKind`]) and the `vyrn why --memory` rows.
//! [`Owned`] answers what owns; the core's placer decides what is released
//! where. Nodes are keyed by [`NodeId`], which a clone keeps.

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
    /// The node the exit is at: a `Block`, the owning `match` / `if let` /
    /// `for in`, a `Stmt::Break` / `Continue` / `Return`, or an `Expr::Try`.
    pub site: NodeId,
    /// The owning node: a `Stmt::Let`, a parameter, a pattern binder, a `for`
    /// body for its variable, or the construct for its temporary.
    pub binding: NodeId,
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

#[derive(Clone, Default)]
pub struct Ownership {
    /// Per function, every `let` in source order and its fate, written by the
    /// placer. Empty without a placer and for a body the core does not lower.
    pub memory: HashMap<FnId, Vec<MemoryRow>>,
    /// The table this analysis decided with, so an explicit `drop x` asks the
    /// same question as the automatic path.
    pub proto: Owned,
    /// Per function, every release step in run order, grouped by
    /// [`Release::site`].
    pub releases: HashMap<FnId, Vec<Release>>,
    /// See [`crate::movecheck::Facts::fnval_clear`]. The core asks it at a call
    /// through a fn value, where no capability row answers.
    pub fnval_clear: std::collections::HashSet<String>,
    /// The program's user projection names ([`crate::project::place_names`]),
    /// which the core reads as element reads.
    pub place_names: std::collections::HashSet<String>,
    /// See [`crate::declared::ArgCaps`].
    pub arg_caps: crate::declared::ArgCaps,
    /// What the kernel decided over the placer's first build of each body,
    /// which every later build of this program writes down. Empty without a
    /// placer.
    pub placed: Placed,
    /// The checker's record this program is lowered against, which the
    /// core's builds and the emitters read a node's type off.
    pub record: std::sync::Arc<crate::checker::Recorded>,
    /// What the placer's effect judgment found each body's callees store
    /// into. Empty outside `vyrn_lower::core::augment` and before its
    /// judgment.
    pub state_callees: StateCallees,
    /// The module-state `String` accumulators of the program the placer
    /// places (`vyrn_lower::append::global_append_candidates`). Empty outside
    /// `augment`.
    pub accumulators: std::collections::BTreeSet<String>,
}

/// Per body name, each callee that may store into module state, with the
/// globals it may store into. The kernel ends a borrow of one of them at the
/// call.
pub type StateCallees = HashMap<String, Vec<(String, Vec<String>)>>;

/// One edge release: the name, the edge, and the holes the release walks
/// around, spelled relative to the name (`Elem.1`).
pub type EdgeRow = (String, u32, Vec<String>);

/// The kernel's placement over a first build. The first build states no
/// release the kernel has not judged owed, so the judgment reports every one.
#[derive(Default, Clone, Debug)]
pub struct Placed {
    /// `(switch site, arm) -> [(binder, holes)]`: the payload binders still
    /// held where their arm ends.
    pub arms: HashMap<(NodeId, u32), Vec<(String, Vec<String>)>>,
    /// Per join node, the releases one edge owes because another edge took
    /// the name. A sub-place row is spelled `d.line`.
    pub edges: HashMap<NodeId, Vec<EdgeRow>>,
    /// The store nodes the kernel found a held place at: the stores that
    /// release what they displace, with the holes each release walks around.
    pub stores: HashMap<NodeId, Vec<String>>,
    /// The nodes that produced a borrowed receiver still held, whose free
    /// rides as an argument-temporary drop.
    pub producers: std::collections::HashSet<NodeId>,
}

/// Holds the checker's record of one program per command, adopting the
/// load's through [`hand_on`], and scopes `vyrn_lower::analyze`'s memo
/// ([`memo_scope`]).
///
/// The guard borrows its program, so no other `Program` can take that address
/// while it is held: a hit is the same program. Any other program analysed
/// inside the guard (a generator's, during a load) misses and is not cached.
/// Only the CLI and the editor open one, beside [`crate::project::Memo`].
pub struct Memo<'a> {
    program: std::marker::PhantomData<&'a Program>,
}

/// A program's identity from the load to the lowering. The `Program` moves in
/// between, but its `functions` buffer does not, so the buffer address plus the
/// two lengths a synthesis can change survive the move.
pub fn ident(program: &Program) -> (usize, usize, usize) {
    (
        program.functions.as_ptr() as usize,
        program.functions.len(),
        program.type_decls.len(),
    )
}

thread_local! {
    /// The load's checker record, and the [`ident`] of its program.
    #[allow(clippy::type_complexity)]
    static LOADED: std::cell::RefCell<
        Option<((usize, usize, usize), Option<crate::checker::HeldRecord>)>,
    > = const { std::cell::RefCell::new(None) };
}

/// Hands the load's checker record to the [`Memo`] the command opens next.
///
/// Only inside a compile scope ([`crate::project::memo_open`]): there a
/// projection site keeps one expansion, so the load's nodes are the ones the
/// command lowers. The editor opens none, because a reused `functions` buffer
/// would make [`ident`] match two different texts.
pub fn hand_on(program: &Program) {
    if !crate::project::memo_open() {
        return;
    }
    let record = crate::checker::held(program);
    LOADED.with(|l| *l.borrow_mut() = Some((ident(program), record)));
}

/// Drops what the load handed on. Call it after rewriting the program in place
/// (`vyrn serve` renames calls), which [`ident`] cannot see.
pub fn forget_loaded() {
    LOADED.with(|l| *l.borrow_mut() = None);
}

thread_local! {
    /// The program the open memo answers for, as an address never
    /// dereferenced, and the count of memos opened on this thread, which
    /// names the open one. `(0, _)` outside a memo.
    static MEMO_FOR: std::cell::Cell<(usize, u64)> = const { std::cell::Cell::new((0, 0)) };
}

/// The open memo's program address and generation, or `None` outside one. A
/// generation names one [`Memo::open`], so an answer cached under it serves
/// no later memo.
pub fn memo_scope() -> Option<(usize, u64)> {
    Some(MEMO_FOR.with(|p| p.get())).filter(|(at, _)| *at != 0)
}

impl<'a> Memo<'a> {
    /// Holds the checker's record of `program` until the guard drops,
    /// adopting the load's when it was made for this program. The CLI must
    /// open [`crate::project::Memo`] before the load, or a projection site is
    /// inlined twice under different tags and the adopted rows key nothing.
    pub fn open(program: &'a Program) -> Memo<'a> {
        MEMO_FOR.with(|p| p.set((program as *const Program as usize, p.get().1 + 1)));
        let adopted = LOADED
            .with(|l| l.borrow_mut().take())
            .filter(|_| crate::project::memo_open())
            .filter(|(id, _)| *id == ident(program))
            .and_then(|(_, r)| r);
        // The lowering reads the checker's record (`checker::recorded`)
        // instead of checking the program again.
        crate::checker::hold_open(program);
        if let Some(record) = adopted {
            crate::checker::adopt(program, record);
        }
        Memo {
            program: std::marker::PhantomData,
        }
    }
}

impl Drop for Memo<'_> {
    fn drop(&mut self) {
        MEMO_FOR.with(|p| p.set((0, p.get().1)));
        crate::checker::hold_close();
    }
}

/// Analyses ownership across a whole program: the plan before the core's
/// placer (`vyrn_lower::analyze`) adds the release rows and the memory rows.
pub fn analyze(program: &Program) -> Ownership {
    let _p = crate::prof::phase("own: analyze_now");
    let ps = crate::prof::phase("own: Owned::new");
    let proto = Owned::new(program);
    drop(ps);
    let fs = crate::prof::phase("own: movecheck::facts");
    let facts = crate::movecheck::facts(program);
    drop(fs);
    Ownership {
        memory: HashMap::new(),
        proto,
        // Only the placer writes release rows.
        releases: HashMap::new(),
        fnval_clear: facts.fnval_clear.clone(),
        place_names: crate::project::place_names(program),
        arg_caps: crate::declared::ArgCaps::new(program),
        placed: Placed::default(),
        record: crate::checker::recorded(program),
        state_callees: HashMap::new(),
        accumulators: Default::default(),
    }
}

/// Groups release steps by `(exit, site)`, each binding with its row's
/// [`Release::holes`], in run order.
pub fn placed(steps: &[Release]) -> HashMap<(Exit, NodeId), Vec<(NodeId, Option<Vec<String>>)>> {
    let mut out: HashMap<(Exit, NodeId), Vec<(NodeId, Option<Vec<String>>)>> = HashMap::new();
    for r in steps {
        out.entry((r.exit, r.site))
            .or_default()
            .push((r.binding, r.holes.clone()));
    }
    out
}
