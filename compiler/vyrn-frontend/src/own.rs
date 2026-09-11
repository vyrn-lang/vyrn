//! Drop **emission** for owned bindings (RFC-0089 rule 4, Phase 4c).
//!
//! This is the *ownership* half of the memory model's Path A — the counterpart
//! to `region` arenas. It decides, per function, three things:
//!
//!   * **droppable** `let` bindings — ones that still own their value where
//!     their block ends, so the backend releases them there; and
//!   * whether the function **transfers** its result, which since rule 3 is the
//!     return type and nothing else.
//!
//! **The rule is one sentence.** Every owning binding that was not moved out
//! releases at scope exit. Both halves come from somewhere else, and that is the
//! point of this phase:
//!
//!   * **What owns** is a property of the type, and [`Owned`] is the only place
//!     it is answered — seeded built-in rows plus every `impl Owned for T` in the
//!     program. There is no second list. It is a reading of the declarations, so
//!     it lives with them, in [`crate::declared`].
//!   * **What moved** is a property of the program's flow, and
//!     [`crate::movecheck`] is the only place it is answered. Rules 1 to 3 are
//!     enforced and last-use aware, so the pass that refuses a use-after-move
//!     already knows, at every store, return, drop and capture, whether a binding
//!     still holds its value.
//!
//! Until Phase 4c this file inferred both. It carried a list of expression forms
//! that "transfer", a list of built-in calls that produce, a list of argument
//! positions that only read, and a fixpoint over which functions return an owned
//! value. Every one of those was a guess made in parallel with a rule the
//! compiler was separately enforcing, and where the guess was unsure it leaked.
//! The lists are gone. What is left is a walk that finds the `let`s, asks the two
//! questions, and writes down the answer.
//!
//! Two conditions are still this file's own, because neither is about the value:
//! a `String` allocated inside a `region` belongs to the arena and must not also
//! be freed, and a `String` literal is data-segment storage that nothing
//! allocated.
//!
//! Identities are `Stmt::Let` node addresses (`*const Stmt as usize`): the
//! backend runs this on the same borrowed AST it emits, so the addresses match
//! one-to-one — a collision-free key where a source line is not (two `let`s can
//! share a line). `movecheck` is keyed the same way and walks the same borrowed
//! AST, which is what lets the two agree by construction.

use std::collections::HashMap;

use crate::ast::*;
use crate::declared::Owned;

/// Which exit runs a release step — RFC-0101 §2.1 item 3 and [A9]'s axis.
///
/// It lives here rather than in `vyrn-lower` because all three engines report
/// against it and the interpreter cannot import that crate. One vocabulary: the
/// form places a step under one of these, each engine reports the walk it runs
/// under the same one, and the gate compares them without a translation table
/// in the middle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Exit {
    /// The fall-through end of a block.
    Block,
    /// The temporary a `match`, `if let` or `for in` OWNS, released where `own`
    /// says the construct is its last owner. The row is keyed by the construct,
    /// and it exists only where no arm handed the payload out — the handover is
    /// the absence of a step, not a step of its own.
    Scrutinee,
    /// `break` — every frame the innermost loop's body opened.
    Break,
    /// `continue` — the same frames as [`Exit::Break`], a different target.
    Continue,
    /// `return` — every frame the function has open.
    Return,
    /// A propagating `?`, which is a function exit and pays what one pays. The
    /// interpreter did not, until RFC-0101 M4's step 0 measured it.
    Try,
}

/// One reclamation the LANGUAGE runs, PLACED rather than asked for — RFC-0101
/// §2.1 item 3.
///
/// [`Ownership::droppable`] answers "is this binding droppable, and nominally
/// how", keyed by node address, and until M4's deletion phase every engine then
/// decided for itself where the answer applies and in what order. rustc's
/// `MirPhase` names the difference: an unelaborated drop is a QUESTION and an
/// elaborated one is an INSTRUCTION. This is the instruction — a place, a kind
/// and an exit, in the order it runs.
///
/// **It lives here rather than in `vyrn-lower`, and that is M4's one deviation
/// from the RFC's own text.** §2.1 puts the steps in the lowered form and M4's
/// consumption phase then has three readers, one of which is the interpreter —
/// which is in this crate and cannot import `vyrn-lower`. The placement is not
/// per-instance anyway: `site`, `binding`, `exit` and the order are properties of
/// a body and `own`'s map, and the only instance-dependent part is the type a
/// [`DropKind::Deep`] walks, which every engine already substitutes at its own
/// emit site. So the placement is computed once here, `vyrn_lower::Instance`
/// carries the substituted view of it, and one order serves all three engines.
#[derive(Debug, Clone)]
pub struct Release {
    /// The node the exit is AT, by node address — the identity `own` and
    /// `movecheck` key on already (RFC-0101 §2.5).
    ///
    /// A `Block` for a fall-through exit; the `match` / `if let` / `for in` for
    /// the temporary a construct owns; the `Stmt::Break` / `Continue` / `Return`
    /// or the `Expr::Try` for an early one. An engine standing at any of those
    /// has the node in hand, so it asks for its steps without re-deriving a
    /// boundary index — which is what `LoopCtx::drop_boundary`, `Fn_::loops`'s
    /// third field and `Flow::Break` propagation were three spellings of.
    pub site: usize,
    /// The node that owns the value — `own`'s own key. A `Stmt::Let` for a
    /// binding; the construct itself for the temporary it owns.
    pub binding: usize,
    /// The binding's name, so a dump reads as the source does.
    pub name: String,
    /// This map's own answer, unsubstituted. See [`Release`] on why the
    /// substitution is the reader's.
    pub kind: DropKind,
    pub exit: Exit,
    pub line: u32,
    /// RFC-0125 M3: the holes THIS row walks around, when the placer decided
    /// them for this exit rather than the analysis for the binding. `None`
    /// means the binding's own set (the `holes` table). The placer sets it
    /// where a name is held with a hole at an exit the analysis placed
    /// nothing at: the hole set at that exit is the kernel's state there,
    /// which may differ from the binding's set on another path.
    pub holes: Option<Vec<String>>,
}

/// How a droppable binding is reclaimed at block exit.
///
/// Not `Copy`: [`DropKind::Release`] carries the name of the method the type
/// declared, which is the point of RFC-0086 M1, and the receiver type it was
/// decided for, which is RFC-0101 M5.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DropKind {
    /// A dynamic `String` — `free` the buffer (Path A).
    FreeStr,
    /// A growable array — free the backing buffer.
    FreeArr,
    /// A `SmallArray<T, N>` (RFC-0056) — free its `data` buffer, which is null
    /// while inline (so `free(null)` is a harmless no-op) and heap once spilled.
    /// Frees iff spilled; the drop site is identical either way.
    FreeSmallArr,
    /// A `Map<String, V>` (RFC-0028) — free both parallel backing buffers
    /// (keys and values). Elements are a safe leak, exactly as for arrays.
    FreeMap,
    /// A `Stream<T>` (RFC-0075 M2b) — the release is variant-aware, so it is one
    /// call to `@__vyrn_stream_close` rather than an inline `free`: a buffer
    /// stream frees its buffer and a stepped one releases its cursor cell, and
    /// which is which is a runtime tag. Keeping the branch in a runtime function
    /// also keeps every drop SITE straight-line, which the early-return path
    /// (`emit_all_drops`, mid-block) depends on.
    CloseStream,
    /// An aggregate the engines copy by value, holding heap in its places
    /// (RFC-0089 rule 4, Phase 5): a record field, a fixed-array slot, an enum or
    /// `Option`/`Result` payload, a closure's capture block. Releasing it releases
    /// them, and the walk is the type — the same walk `copy` already makes, with
    /// `free` where `copy` has `malloc`.
    ///
    /// It carries the type because the shape is not one offset list: a variant
    /// payload is selected at run time, and only the live variant is released.
    Deep(Type),
    /// A type that declared `impl Owned for T` (RFC-0086 M1) — call its own
    /// `release`, whose flattened name this carries. The compiler emits an
    /// ordinary call, so a third party's container is reclaimed by the same
    /// mechanism a built-in is, in the same words, with no compiler patch.
    ///
    /// It carries the RECEIVER TYPE the name was decided for, and that second
    /// member is RFC-0101 M5. A flattened `impl<T> Owned for Slots<T>` is a
    /// GENERIC function, so the name alone does not say which instance a step
    /// reaches: an emitter parked the value under a reserved binding and went
    /// through the ordinary call path to have the parameters solved from the
    /// receiver, and nothing above a backend could work out that the body was
    /// wanted. That was the whole of the `ImplicitDispatch` class M2 named and
    /// M4 measured at 24 — every one of them `Owned__Slots__release<…>`.
    /// The type is the one [`Owned::release_kind`] was ASKED about, unresolved
    /// and unsubstituted, which is exactly what both backends already pass
    /// beside the name (`Rel::Call`, `Gen::call_release`); a reader that wants
    /// it per instance substitutes it, as it already does for [`DropKind::Deep`].
    Release(String, Type),
}

impl DropKind {
    /// How this kind reclaims, in words.
    ///
    /// One source for two surfaces: `vyrn why --memory` prints it at the shell
    /// and the LSP shows it on hover (RFC-0087 U1). A second wording would be a
    /// second answer.
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

/// Which row gives a type its must-use obligation — the one thing the two rows
/// do not share, because they are discharged differently.
///
/// The obligation itself is identical: acquired once, disposed exactly once,
/// proved on every path. What differs is the menu a diagnostic offers, and a
/// wrong menu is worse than a vague one — `drop s` on a `Stream` reclaims
/// nothing, because a stream's release is pushed by its own lowering
/// (RFC-0075 M2b) and [`Owned::release_kind`] answers `None` for it on purpose.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Linear {
    /// The seeded row: a `Stream<T>`, consumed with `for … in`, forwarded by
    /// returning it, or released with `close(s)`.
    Stream,
    /// A declared `impl MustUse for T` row. The value is handed on by name — to
    /// a call, or to the return — or released with `drop t`, which runs whatever
    /// `impl Owned for T` declared.
    ///
    /// It carries the type key that DECLARED the row, which since RFC-0092 M4 is
    /// not always the type asked about: an `Array<Txn>` is obliged because `Txn`
    /// is, and a note that said `Array<Txn>` declares it would name a row no
    /// program wrote.
    Declared(String),
}

/// One `let` binding and what happens to its value — the row behind `vyrn why
/// --memory` and the editor's memory hints (RFC-0087 U1).
///
/// The prose is already rendered, because the pass that DECIDED a binding's
/// ownership is the pass that words it: the core, through the placer slot
/// (`vyrn_lower::core`, RFC-0125 §3 M3, the report slice). This crate states
/// no rule about a named binding's fate any more, so there is no second
/// opinion left to disagree with the first.
#[derive(Clone, Debug)]
pub struct MemoryRow {
    pub name: String,
    /// 1-based line of the `let`.
    pub line: usize,
    /// What happens to the value, in one line.
    pub text: String,
    /// The line where the value stops being live, when there is one: a move
    /// or a `drop`. `None` for a binding that lives to block exit.
    pub last_use: Option<usize>,
    /// What took it, for the inlay hint. `Some` exactly when the value moved.
    pub moved_into: Option<String>,
    /// Which of the report's six counters this row falls in.
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
    /// Not reclaimed. `reason` is the row with its lines and names removed,
    /// so a corpus of them groups; `heap` says whether the type owns heap at
    /// all, because a scalar has nothing to reclaim and the editor writes no
    /// hint about one.
    Leaked {
        reason: &'static str,
        heap: bool,
    },
}

/// What is left of RFC-0114 §26's artifact: node IDENTITY, and no release
/// decision at all.
///
/// Every table this struct carried has gone with the emitter reader that
/// asked it (RFC-0125 §3 M3, the emitter-reads-the-core-alone slice). What
/// remains is the alias map, which answers a question no pass states: a
/// user-container `for` and a `place at` rewrite CLONE the statements they
/// expand, so the core's answers are filed under nodes the emission never
/// walks, and [`ReleasePlan::key_of`] resolves a clone back to the node the
/// core keyed. That is the next payer named in §3 M3.
#[derive(Clone, Default)]
pub struct ReleasePlan {
    /// Clone-to-original address pairs from `project::iterate_loop` (RFC-0114
    /// §26): a user-container `for` clones its body, so the core's answers
    /// live on nodes the emission never walks — every lookup resolves through
    /// this map first, chaining for a clone of a clone (a nested loop).
    alias: std::cell::RefCell<HashMap<usize, usize>>,
    /// The keys [`ReleasePlan::alias_clones_scoped`] added, in order — what
    /// [`ReleasePlan::alias_unwind`] removes. A TRANSIENT clone (a rewrite's
    /// synthesized tree, a higher-order call's argument list) dies with its
    /// site, and an alias that outlived it would fire on whatever later node
    /// the allocator hands the same address.
    alias_log: std::cell::RefCell<Vec<usize>>,
}

impl ReleasePlan {
    /// Register clone→original pairs for a clone that LIVES as long as the
    /// compile — `iterate_loop`'s leaked expansions, a queued shell's body.
    pub fn alias_clones(&self, pairs: &[(usize, usize)]) {
        self.alias.borrow_mut().extend(pairs.iter().copied());
    }

    /// Register pairs for a TRANSIENT clone, to be removed by
    /// [`ReleasePlan::alias_unwind`] when the clone dies.
    pub fn alias_clones_scoped(&self, pairs: &[(usize, usize)]) {
        self.alias.borrow_mut().extend(pairs.iter().copied());
        self.alias_log
            .borrow_mut()
            .extend(pairs.iter().map(|(c, _)| *c));
    }

    /// The watermark [`ReleasePlan::alias_unwind`] rolls back to.
    pub fn alias_scope(&self) -> usize {
        self.alias_log.borrow().len()
    }

    /// Remove every scoped alias registered since `mark` — called where the
    /// transient clone goes out of scope.
    pub fn alias_unwind(&self, mark: usize) {
        let mut log = self.alias_log.borrow_mut();
        let mut map = self.alias.borrow_mut();
        for k in log.drain(mark..) {
            map.remove(&k);
        }
    }

    /// The plan-bearing address `at` stands for: itself, or — through the
    /// alias map, chained for nested clones — the original node it copies.
    fn resolve(&self, mut at: usize) -> usize {
        let alias = self.alias.borrow();
        // Chained lookups are bounded by clone nesting depth; the guard is
        // against a cycle that a defect in the pair builder could create.
        for _ in 0..64 {
            match alias.get(&at) {
                Some(next) => at = *next,
                None => break,
            }
        }
        at
    }

    /// The node a core answer is keyed by, for a reader that walks a CLONE
    /// of the statement the core judged (RFC-0125 §3 M3, the
    /// deletion-preparation slice).
    pub fn key_of(&self, at: usize) -> usize {
        self.resolve(at)
    }
}

/// Whole-program ownership facts.
#[derive(Clone, Default)]
pub struct Ownership {
    /// The per-node release decisions — see [`ReleasePlan`].
    pub plan: ReleasePlan,
    /// Per function: every `let` in source order, and what happens to its
    /// value, in the words `vyrn why --memory` and the editor print
    /// (RFC-0087 U1).
    ///
    /// Written by the CORE, through the placer slot: it is the pass that
    /// states a named binding's ownership, so it is the pass that words the
    /// report (RFC-0125 §3 M3, the report slice). Empty where no placer is
    /// installed (`VYRN_NO_PLACER=1`) and for a body the core does not lower.
    pub memory: HashMap<String, Vec<MemoryRow>>,
    /// The `Owned` table this analysis decided with. Handed out so a backend
    /// lowering an explicit `drop x` asks the SAME question the automatic path
    /// asked, instead of keeping a second copy of the answer.
    pub proto: Owned,
    /// Per function: [`droppable`](Ownership::droppable)'s rows PLACED — every
    /// step, at the exit that runs it, in the order it runs (RFC-0101 M4).
    ///
    /// Grouped by [`Release::site`], which is the node the exit is at. This is
    /// the one order that used to be asserted separately by `Gen::drop_stack`,
    /// `Fn_::releases` and the interpreter's per-block `Vec`.
    pub releases: HashMap<String, Vec<Release>>,
    /// Round forty-six's meet, by signature key — see
    /// [`crate::movecheck::Facts::fnval_clear`]. The fourth answer only a
    /// pass that has read every body can give, and the core asks it at a call
    /// through a fn value, where no capability row answers.
    pub fnval_clear: std::collections::HashSet<String>,
    /// The capability of every declared position, by callee name — see
    /// [`crate::declared::arg_caps`].
    ///
    /// It is a read of the DECLARATIONS and says nothing about a body, so it
    /// is the same table for every body of the program. The core used to build
    /// it per body, and a program with hundreds of functions paid the whole
    /// declaration list once for each of them (RFC-0125 §3 M3, the placer's
    /// cost). It sits beside `lending` and `retains` because the core asks all
    /// three at the same position, in [`crate::movecheck::arg_verdict`].
    pub arg_caps: HashMap<String, Vec<Capability>>,
}

/// One analysis per command — RFC-0125 §3 M3, the repetition slice.
///
/// [`analyze`] runs the placer, which builds a core body for every instance of
/// the LINKED program and judges it. The load asks for one so `kernel_refuses`
/// can print this program's refusals, and the engine that lowers or emits used
/// to ask for a second. The second answer was the first one recomputed. It is
/// now the first one, handed on by [`hand_on`] and adopted here.
///
/// The guard BORROWS the program it caches for, so the program outlives the
/// guard and no other `Program` can take that address while the entry is held.
/// That is why an address is a sound key here, and it is the whole proof: a hit
/// is the same program, so it is the same answer. Any other program analysed
/// inside the guard — a generator's own, during a load — has a different
/// address, misses, and is neither served from the cache nor written to it.
///
/// Opened by the CLI beside [`crate::project::Memo`], after the load and for
/// the one program the command is about. Nothing else opens one, so a host that
/// does not arm it analyses twice as before.
pub struct Memo<'a> {
    program: std::marker::PhantomData<&'a Program>,
}

/// A program's identity, from the point the load judges it to the point the
/// command lowers it.
///
/// The `Program` STRUCT moves in between — the CLI's load returns it by value —
/// so its address is not one. The heap buffer behind `functions` does not move
/// with it, and no two live programs share a buffer, so its address plus the
/// two lengths a synthesis can change is an identity that survives the move.
fn ident(program: &Program) -> (usize, usize, usize) {
    (
        program.functions.as_ptr() as usize,
        program.functions.len(),
        program.type_decls.len(),
    )
}

thread_local! {
    /// The analysis the LOAD made, and the identity of the program it was made
    /// for. [`Memo::open`] adopts it when the two agree.
    static LOADED: std::cell::RefCell<Option<((usize, usize, usize), Ownership)>> =
        const { std::cell::RefCell::new(None) };
}

/// Hand the load's analysis on to the guard the command opens next.
///
/// Called by [`crate::movecheck::refusals`], which is the one analysis a
/// judgment may be reused for.
///
/// **Only inside a compile scope** ([`crate::project::memo_open`]), and
/// [`Memo::open`] adopts under the same condition. That scope is the proof
/// [`ident`] cannot give on its own: the analysis is about NODES, and only
/// inside it does a projection site keep one expansion, so only inside it are
/// the load's nodes the ones the command lowers. It also bounds who can be
/// wrong. The editor opens no compile scope — it re-checks a program per
/// keystroke, drops it, and builds the next one, and an allocator that hands
/// the same `functions` buffer to a program of the same shape would make
/// `ident` agree about two different texts.
pub fn hand_on(program: &Program, ownership: &Ownership) {
    if !crate::project::memo_open() {
        return;
    }
    LOADED.with(|l| *l.borrow_mut() = Some((ident(program), ownership.clone())));
}

/// Drop what the load handed on, because this program is no longer the one the
/// load judged.
///
/// One caller: `vyrn serve` rewrites call names and one function's name in
/// place after the load. Neither shows in [`ident`], and an analysis of the
/// program before the rewrite is not an analysis of the program after it.
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
    /// Hold one analysis of `program` until the guard is dropped, adopting the
    /// load's if the load made one for this program.
    ///
    /// The load makes an analysis of its own — the ownership stage judges every
    /// program it checks (RFC-0125 §3 M3, the accumulation slice) — and this
    /// guard now takes it. It could not before: a projection is inlined into
    /// its caller's block with a per-inline tag (`project.rs`), so the analysis
    /// the load made named bindings `@p26.h` and the lowering a tool ran next
    /// named them `@p31.h`; a plan whose rows are keyed by those nodes then
    /// placed nothing, and `examples/genref.vyrn` leaked a block. What closed
    /// it is scope, not a new tag: the CLI opens [`crate::project::Memo`]
    /// BEFORE the load, so a site is inlined once for the whole command and
    /// both readings walk the same nodes.
    pub fn open(program: &'a Program) -> Memo<'a> {
        MEMO_FOR.with(|p| p.set(program as *const Program as usize));
        let adopted = LOADED
            .with(|l| l.borrow_mut().take())
            .filter(|_| crate::project::memo_open())
            .filter(|(id, _)| *id == ident(program))
            .map(|(_, o)| o);
        MEMO.with(|m| *m.borrow_mut() = adopted);
        // RFC-0125 §3 M3, the one check: what the checker decided about every
        // node of this program, for the same span and on the same proof. The
        // lowering reads it (`checker::recorded`) instead of checking the
        // program a second time.
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

/// Analyse ownership across a whole program.
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
    // What every `let` in the program still owns where its block ends, decided
    // by the pass that enforces the rules. One walk, one answer, no second
    // opinion (RFC-0087 records three defects that were two walkers disagreeing).
    let fs = crate::prof::phase("own: movecheck::facts");
    let facts = crate::movecheck::facts(program);
    drop(fs);
    let plan = ReleasePlan::default();
    let mut ownership = Ownership {
        plan,
        memory: HashMap::new(),
        proto,
        // Every row in this table is the placer's now: the analysis injects
        // none, and the fold that did is deleted (RFC-0125 §3 M3).
        releases: HashMap::new(),
        fnval_clear: facts.fnval_clear.clone(),
        arg_caps: crate::declared::arg_caps(program),
    };
    // RFC-0125 M3: the placer, when one is installed, adds the release rows
    // this analysis owes and did not place. It runs the lowering, which runs
    // this analysis, so it is not re-entered.
    if let Some(place) = PLACER.get() {
        if !PLACING.with(|p| p.get()) {
            PLACING.with(|p| p.set(true));
            place(program, &mut ownership);
            PLACING.with(|p| p.set(false));
        }
    }
    ownership
}

/// The plan's key for a `for` variable, which has no `let` node: the heap
/// buffer of its spelling in the statement (RFC-0125 M3). Not the `String`'s
/// own address: that is the first field of `Stmt::ForIn`, at offset 0 under
/// a niche-encoded discriminant, so it equals the statement's address, which
/// is the container row's key — and the two rows overwrote each other.
pub fn for_var_key(var: &str) -> usize {
    var.as_ptr() as usize
}

/// The plan's key for a pattern BINDER, which has no `let` node either: the
/// heap buffer of the name the reader wrote in the pattern, for the same
/// reason and with the same caveat as [`for_var_key`].
///
/// A binder the arm OWNS is a binding of the frame like any other, and the
/// frame's exit rule releases it at a `return`, a `break` or a `continue`
/// inside the arm. Both the core and the emitters take the key off the same
/// pattern node, so a row placed against it lands where the release is
/// emitted (RFC-0125 §3 M3, the walk's deletion).
pub fn binder_key(name: &str) -> usize {
    name.as_ptr() as usize
}

/// A pass that adds release rows to a finished analysis — RFC-0125 M3's
/// placer over the named core, which lives in `vyrn-lower` and cannot be
/// named from here.
pub type Placer = fn(&Program, &mut Ownership);

static PLACER: std::sync::OnceLock<Placer> = std::sync::OnceLock::new();

thread_local! {
    static PLACING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Install the placer. The first installation wins; a second is ignored.
pub fn install_placer(f: Placer) {
    let _ = PLACER.set(f);
}

/// Whether a placer is installed — whether, that is, anything in this process
/// lowers a program. It is the one reader of what the checker records
/// (RFC-0125 §3 M3, the one check), so the analysis's check asks this before it
/// keeps a record nobody would read.
pub fn placer_installed() -> bool {
    PLACER.get().is_some()
}

/// The hard refusals the kernel made about the program the placer just judged,
/// drained, as diagnostics — RFC-0125 §3 M3, the accumulation slice.
///
/// The second slot of the same shape as [`Placer`], and for the same reason: the kernel lives in `vyrn-lower`, this crate sits below it, and
/// a refusal has to reach the one list a file's refusals come out in
/// ([`crate::movecheck::refusals`]). Draining is the point — the loader runs a
/// generator by loading a whole program of its own, and each such load takes
/// its own refusals with it, so what is left is this program's.
pub type Refusals = fn() -> Vec<crate::diagnostics::Diagnostic>;

static REFUSALS: std::sync::OnceLock<Refusals> = std::sync::OnceLock::new();

/// Install the kernel's refusal drain. The first installation wins.
pub fn install_refusals(f: Refusals) {
    let _ = REFUSALS.set(f);
}

/// What the kernel refuses about the program just analysed. Empty where
/// nothing is installed — a host that never linked the lowering, or
/// `VYRN_NO_KERNEL=1`, which the drain itself answers for.
pub fn kernel_refusals() -> Vec<crate::diagnostics::Diagnostic> {
    REFUSALS.get().map(|f| f()).unwrap_or_default()
}

/// The must-use judgment — RFC-0125 §3 M3, the obligation slice.
///
/// The fourth slot of the same shape, and for the same reason as [`Refusals`]:
/// the rule is a TYPE's obligation and not an ownership one, so it left
/// `movecheck.rs` for the typed judgment (`vyrn_lower::typed::obligation`),
/// which this crate sits below. Unlike the kernel's drain this one is asked of
/// a PROGRAM: the walk reads the tree a reader wrote and holds no state
/// between calls.
pub type MustUse = fn(&Program) -> Vec<crate::diagnostics::Diagnostic>;

static MUST_USE: std::sync::OnceLock<MustUse> = std::sync::OnceLock::new();

/// Install the must-use judgment. The first installation wins.
pub fn install_must_use(f: MustUse) {
    let _ = MUST_USE.set(f);
}

/// What the must-use judgment refuses about `program`. Empty where nothing is
/// installed — a host that never linked the lowering.
pub fn must_use_refusals(program: &Program) -> Vec<crate::diagnostics::Diagnostic> {
    MUST_USE.get().map(|f| f(program)).unwrap_or_default()
}

/// The placement as a consumer reads it: `(exit, the node the exit is AT)` maps
/// to the bindings released there, in the order they run.
///
/// One reader for three engines. An engine standing at an exit has the node in
/// hand, looks its steps up, and maps each binding to whatever it releases a
/// value WITH — an alloca name, a wasm place, a scope entry. What it never does
/// again is decide the order, or derive a boundary index to find where its own
/// frames stop.
///
/// The second element is the hole set the step walks around, when the row
/// carries its own: the placer's rows (RFC-0125 M3) carry the kernel's set at
/// that exit. `None` leaves the binding's own set in force.
pub fn placed(steps: &[Release]) -> HashMap<(Exit, usize), Vec<(usize, Option<Vec<String>>)>> {
    let mut out: HashMap<(Exit, usize), Vec<(usize, Option<Vec<String>>)>> = HashMap::new();
    for r in steps {
        out.entry((r.exit, r.site))
            .or_default()
            .push((r.binding, r.holes.clone()));
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{lexer::lex, parser::parse};
    use std::collections::HashSet;

    fn analyze_src(src: &str) -> (Ownership, Program) {
        let p = parse(lex(src).unwrap()).unwrap();
        let o = analyze(&p);
        (o, p)
    }

    // ---- ownership transfer ---------------------------------------------

    // ---- shadowing (an inner binder is not the outer binding) ------------

    /// RFC-0089 rule 3, Phase 4c. A return is owned, so the return TYPE is the
    /// whole answer and the fixpoint that used to look for a borrowed return
    /// path asked a question the language now answers. The kernel refuses the
    /// programs that fixpoint used to describe (`return s` on a `read`
    /// parameter).
    ///
    /// The table it left behind — `Ownership::owned_fns`, a name-keyed copy of
    /// `release_kind(&f.ret)` — went with it (RFC-0125 §3 M3, the
    /// ownership-file slice). What is asserted is the question its one reader
    /// asks now.
    #[test]
    fn a_heap_return_type_always_transfers() {
        let src = "fn make(a: String, b: String) -> String { return a + b; } \
                   fn count(s: String) -> Int64 { return s.byteLength; } \
                   fn main() -> Int64 { return 0; }";
        let (o, _) = analyze_src(src);
        assert_eq!(
            o.proto.release_kind(&Type::Str),
            Some(DropKind::FreeStr),
            "`make`'s return type"
        );
        assert_eq!(o.proto.release_kind(&Type::Int), None, "`count`'s");
    }

    // ---- census §14 at a `match`: the scrutinee and its payload ----------

    // ---- auto-free for mutable arrays -----------------------------------

    // ---- the RFC-0089 gate (M0) ------------------------------------------

    /// The RFC-0089 rule-1 predicate, now a public function so the checker and
    /// both backends ask it too (`copy` copies exactly what this counts).
    fn owns_heap(ty: &Type, types: &HashMap<String, TypeDecl>, _depth: usize) -> bool {
        crate::declared::owns_heap(ty, types)
    }

    /// Every `.vyrn` under a repo-relative directory.
    ///
    /// `pub(crate)` so the RFC-0089 gates measure ONE corpus: `movecheck`'s
    /// Phase-4a site census walks exactly the files this one does.
    pub(crate) fn sources(rel: &str, out: &mut Vec<std::path::PathBuf>) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(rel);
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "vyrn") {
                    out.push(p);
                }
            }
        }
    }

    /// RFC-0089 M0's go/no-go evidence: how large the move-error surface is over
    /// the whole corpus, and how much the current analysis leaks.
    ///
    /// It parses each file ALONE — no loader, no linking. That under-counts a
    /// cross-module call's transfer, and it is the only reading that gives one
    /// number per source line rather than one per import graph.
    ///
    /// Ignored by default: it reads the repository, so it is a measurement, not
    /// a unit test. Run it with
    /// `cargo test -p vyrn-frontend rfc0089 -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn rfc0089_move_surface_over_the_corpus() {
        let mut files = Vec::new();
        sources("examples", &mut files);
        sources("std", &mut files);
        files.sort();

        let (mut lines, mut parsed) = (0, 0);
        let (mut param_returns, mut aliases): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
        let reasons: HashMap<&'static str, usize> = HashMap::new();
        let mut total = 0usize;

        for path in &files {
            let Ok(src) = std::fs::read_to_string(path) else {
                continue;
            };
            lines += src.lines().count();
            let Ok(tokens) = crate::lexer::lex(&src) else {
                continue;
            };
            let (program, errs) = crate::parser::parse_accum(tokens);
            if !errs.is_empty() {
                continue;
            }
            parsed += 1;
            let types = crate::types::decl_map(&program);
            let where_ = path.file_name().unwrap().to_string_lossy().to_string();

            for f in &program.functions {
                // Rule 3: a return of a BORROWED parameter from a function whose
                // result owns heap. Returning a local is a legal move and stays
                // legal, and so is returning a `consume` parameter — that is one
                // of the two fixes, so counting it as a site made the migrated
                // corpus look unmigrated. Phase 4b corrected the counter.
                if owns_heap(&f.ret, &types, 0) {
                    let names: HashSet<&str> = f
                        .params
                        .iter()
                        .filter(|p| p.capability != Capability::Consume)
                        .map(|p| p.name.as_str())
                        .collect();
                    for (line, name) in returned_params(&f.body, &names) {
                        param_returns.push(format!("{where_}:{line} {}: return {name}", f.name));
                    }
                }
                // Rule 1: a bare alias of a value that owns heap. Only a
                // `let y = x` whose type this pass can name counts; an unnamed
                // one is invisible to any reading short of the checker.
                for (line, y, x) in bare_aliases(&f.body, &f.params, &types) {
                    aliases.push(format!("{where_}:{line} {}: let {y} = {x}", f.name));
                }
            }

            // The bindings this walk sees. Which of them a frame RECLAIMS is
            // the placer's row and no table here (RFC-0125 §3 M3, the
            // container slice) — `vyrn why --memory` over the same corpus is
            // where the reasons are counted.
            for f in &program.functions {
                walk_stmts(&f.body, &mut |s| {
                    if matches!(s, Stmt::Let { .. }) {
                        total += 1;
                    }
                });
            }
        }

        let mut rows: Vec<_> = reasons.into_iter().collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1));
        let leaks: usize = rows.iter().map(|(_, c)| c).sum();
        println!(
            "corpus: {} files ({parsed} parsed), {lines} lines",
            files.len()
        );
        println!(
            "RFC-0089 rule 3 — returns of a parameter: {}",
            param_returns.len()
        );
        for s in &param_returns {
            println!("    {s}");
        }
        println!(
            "RFC-0089 rule 1 — bare aliases of an owning type: {}",
            aliases.len()
        );
        for s in &aliases {
            println!("    {s}");
        }
        println!("move surface: {}", param_returns.len() + aliases.len());
        println!("bindings: {total}");
        let _ = (leaks, rows);
    }

    /// Every `return p` in `body` that names one of `params`, with its line.
    fn returned_params(body: &Block, params: &HashSet<&str>) -> Vec<(usize, String)> {
        let mut out = Vec::new();
        walk_stmts(body, &mut |s| {
            if let Stmt::Return {
                value: Some(Expr::Var { name, .. }),
                line,
            } = s
            {
                if params.contains(name.as_str()) {
                    out.push((*line, name.clone()));
                }
            }
        });
        out
    }

    /// How many `let y = x` in `body` alias a value whose type owns heap.
    ///
    /// The type comes from the parameter list or from a `let`'s annotation —
    /// the same declared-types reading `expr_type` does, and it under-counts
    /// for the same reason.
    fn bare_aliases(
        body: &Block,
        params: &[Param],
        types: &HashMap<String, TypeDecl>,
    ) -> Vec<(usize, String, String)> {
        // A `consume` parameter is already owned, so aliasing it is a legal
        // move, not a site.
        let mut known: HashMap<String, Type> = params
            .iter()
            .filter(|p| p.capability != Capability::Consume)
            .map(|p| (p.name.clone(), p.ty.clone()))
            .collect();
        let mut out = Vec::new();
        walk_stmts(body, &mut |s| {
            if let Stmt::Let {
                name,
                ty,
                value,
                line,
                ..
            } = s
            {
                if let Expr::Var { name: src, .. } = value {
                    if known.get(src).is_some_and(|t| owns_heap(t, types, 0)) {
                        out.push((*line, name.clone(), src.clone()));
                    }
                }
                if let Some(t) = ty {
                    known.insert(name.clone(), t.clone());
                }
            }
        });
        out
    }

    /// Every statement in a block, nested blocks included.
    fn walk_stmts(b: &Block, f: &mut impl FnMut(&Stmt)) {
        for s in &b.stmts {
            f(s);
            match s {
                Stmt::If {
                    then_block,
                    else_block,
                    ..
                }
                | Stmt::IfLet {
                    then_block,
                    else_block,
                    ..
                } => {
                    walk_stmts(then_block, f);
                    if let Some(eb) = else_block {
                        walk_stmts(eb, f);
                    }
                }
                Stmt::While { body, .. } | Stmt::ForIn { body, .. } | Stmt::Region { body, .. } => {
                    walk_stmts(body, f)
                }
                _ => {}
            }
        }
    }
}
