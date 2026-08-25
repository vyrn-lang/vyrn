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
//!     program. There is no second list.
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
use crate::movecheck::{Gone, LetOwnership};

/// What the INTERPRETER actually releases at a scope exit, made readable —
/// RFC-0101 M4.
///
/// **It used to have three sites and now it has one, which is the deletion phase
/// visible from outside.** The shadow phases made all three engines report their
/// sequence so one gate could assert it against the placement. Both compiled
/// backends READ that placement now, so comparing what they emit against it is
/// comparing a value with itself. This engine still derives its own order, for
/// the reason RFC-0101 §3 M4's ledger records, so it is still the one that needs
/// gating — against `vyrn-cli/tests/lowered.rs`'s fixtures, because its walk
/// happens when a block RUNS.
///
/// It lives here, in the file that DECIDES what is droppable, because the
/// interpreter is in this crate and cannot import `vyrn_codegen::observe`.
///
/// Off by default, thread-local, and every hook records a step the engine was
/// about to take anyway. Nothing here decides anything.
pub mod trace {
    pub use super::Exit;

    /// One exit's release walk, as the interpreter ran it.
    ///
    /// The whole walk rather than one record per release, because the thing being
    /// gated is the SEQUENCE: an exit that releases nothing is a fact too, and a
    /// per-release trace cannot tell it apart from an exit the engine never
    /// reached.
    ///
    /// **One record per exit, however many frames it crosses.** A compiled
    /// backend walks every frame above a boundary index in one call. The
    /// interpreter's walk happens as a signal propagates outward through
    /// `Interp::block`, one frame at a time, so each frame [`joining`]s the walk
    /// the signal opened and appends to it. Concatenating in emission order
    /// instead does NOT work, and the fixture that found it out is worth the
    /// sentence: a release is ordinary Vyrn, so running one emits the callee's
    /// own block exits BETWEEN two frames of the walk being recorded.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Walk {
        /// Which exit kind this walk belongs to.
        pub exit: Exit,
        /// The node the exit is AT: the `Block` for a fall-through exit, the
        /// `match` / `if let` / `for in` for a construct's own temporary, and the
        /// `Stmt::Break` / `Continue` / `Return` or `Expr::Try` for an early one.
        /// It is the key a consumer looks the placed steps up by, so nothing
        /// downstream re-derives a boundary index.
        pub at: usize,
        /// The bindings released, in the order the engine releases them.
        pub bindings: Vec<usize>,
    }

    thread_local! {
        static ON: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        static WALKS: std::cell::RefCell<Vec<Walk>> = const {
            std::cell::RefCell::new(Vec::new())
        };
        static LEAVING: std::cell::Cell<(Exit, usize)> =
            const { std::cell::Cell::new((Exit::Block, 0)) };
        /// Where the walk the current signal is unwinding lives, or `NONE`.
        static OPEN: std::cell::Cell<usize> = const { std::cell::Cell::new(NONE) };
    }

    /// No walk is open — a `usize` rather than an `Option` so the cell is `Copy`
    /// and the hot path is a compare.
    const NONE: usize = usize::MAX;

    /// Start recording on this thread, discarding anything already collected.
    pub fn start() {
        WALKS.with(|s| s.borrow_mut().clear());
        OPEN.with(|c| c.set(NONE));
        ON.with(|o| o.set(true));
    }

    /// Stop recording and take what was collected.
    pub fn take() -> Vec<Walk> {
        ON.with(|o| o.set(false));
        WALKS.with(|s| std::mem::take(&mut *s.borrow_mut()))
    }

    pub fn on() -> bool {
        ON.with(|o| o.get())
    }

    /// Take back what a thread of the engine's own collected.
    ///
    /// The interpreter runs its program on a dedicated stack
    /// (`interp::on_deep_stack`), so a thread-local sink would be left behind on
    /// that thread. The recording is per thread on purpose — two tests in one
    /// binary run in parallel and a global would interleave them — so the thread
    /// that made the stack hands the rows back to the one that asked.
    pub fn adopt(rows: Vec<Walk>) {
        WALKS.with(|s| s.borrow_mut().extend(rows));
    }

    /// Name the exit an unwinding walk belongs to, at the point the SIGNAL is
    /// made.
    ///
    /// Both compiled backends emit an early exit's walk at the site, with the
    /// node in hand. The interpreter's walk happens as `Flow::Break` or
    /// `Ctrl::Return` propagates outward, and neither signal carries a node — so
    /// the site is left here by the statement that raised it, and each unwinding
    /// frame reads it. Recording only; nothing the engine does depends on it.
    pub fn leaving(exit: Exit, at: usize) {
        if on() {
            LEAVING.with(|c| c.set((exit, at)));
            OPEN.with(|c| c.set(NONE));
        }
    }

    /// Reserve this frame's place in the walk the current signal is unwinding.
    ///
    /// Called at the frame's ENTRY, before any release runs, and that is the
    /// whole trick: a release is ordinary Vyrn, so running one pushes the
    /// callee's own block exits into the log between this frame and the next.
    /// A place reserved first survives them.
    pub fn joining() -> usize {
        if !on() {
            return NONE;
        }
        let open = OPEN.with(|c| c.get());
        if open != NONE {
            return open;
        }
        let (exit, at) = LEAVING.with(|c| c.get());
        WALKS.with(|s| {
            let mut v = s.borrow_mut();
            v.push(Walk {
                exit,
                at,
                bindings: Vec::new(),
            });
            let idx = v.len() - 1;
            OPEN.with(|c| c.set(idx));
            idx
        })
    }

    /// Add what one frame released to the place it reserved.
    pub fn joined(slot: usize, bindings: Vec<usize>) {
        if slot == NONE {
            return;
        }
        WALKS.with(|s| s.borrow_mut()[slot].bindings.extend(bindings));
        // The next frame out belongs to the SAME walk, whatever a release
        // running under this one did to the cell.
        OPEN.with(|c| c.set(slot));
    }

    /// Note one frame's worth of one exit's release walk.
    pub fn note(exit: Exit, at: usize, bindings: Vec<usize>) {
        if !on() {
            return;
        }
        WALKS.with(|s| s.borrow_mut().push(Walk { exit, at, bindings }));
    }
}

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
    /// The other seeded row (RFC-0095 M1): a `Task<T>`, joined with `t.join()`,
    /// which yields the result, forwarded by returning it, or released with
    /// `drop t`, which waits for the task and then throws the result away.
    ///
    /// A task owns a frame, a record and an operating-system handle, and the
    /// handle is why the obligation is worth its line: bytes are a leak a
    /// program can live with, and a per-process handle ceiling is a server that
    /// stops.
    Task,
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

/// The `Owned` protocol: **how a type is released**, and the only place that
/// question is answered (RFC-0086 M1).
///
/// The lookup is uniform. The built-in *entries* are **seeded** by the compiler
/// rather than read from `std/`, because `vyrn run` on a bare file has no
/// resolver and therefore no `std/` — RFC-0080 M3 refused to route `?` through a
/// std protocol for exactly that reason, and the same reason applies to the
/// decision that frees memory. A user adds rows with `impl Owned for T`; a bare
/// file keeps working and a third party still joins.
///
/// *Representation* stays intrinsic: `Array`'s three words are primitive, so a
/// built-in row lowers to an inline `free` rather than a protocol call. That is
/// lowering, not deciding. What is declared is the property.
/// `Default` is the seed with no declared rows and no nominal declarations —
/// what a program of built-ins alone would ask.
#[derive(Default)]
pub struct Owned {
    /// One row per `impl Owned for T`: the type key -> its flattened `release`.
    impls: HashMap<String, String>,
    /// One row per `impl MustUse for T` (RFC-0086 M3): the type key alone, since
    /// the obligation declares no method. See [`Owned::must_use`].
    linear: std::collections::HashSet<String>,
    /// Nominal declarations, so `type Email = String` answers as a String does.
    types: HashMap<String, TypeDecl>,
}

impl Owned {
    /// Read the program's `impl Owned` rows and seed the built-in ones.
    pub fn new(program: &Program) -> Self {
        let impls = program
            .impls
            .iter()
            .filter(|i| i.protocol == crate::types::OWNED)
            // A GENERIC impl head carries a row like any other since Phase 8b.
            // The key is the type CONSTRUCTOR (`Slots`), exactly as every other
            // protocol keys one, and the flattened `release` is a generic
            // function. Each drop site solves the type arguments from the
            // binding's own type and asks for that instance — the same route a
            // written call takes. Before 8b the row was filtered out, because a
            // drop site emitted the flattened name unmangled and clang reported
            // the missing symbol at the end of a build.
            .filter_map(|i| crate::types::type_key(&i.ty))
            .map(|k| {
                let m = crate::types::impl_method_name(
                    crate::types::OWNED,
                    &k,
                    crate::types::OWNED_RELEASE,
                );
                (k, m)
            })
            .collect();
        Owned {
            impls,
            // The same read, one protocol over. A GENERIC head carries a row for
            // the reason Phase 8b gave above: the key is the type CONSTRUCTOR,
            // so `impl<T> MustUse for Pool<T>` obliges every instantiation.
            linear: program
                .impls
                .iter()
                .filter(|i| i.protocol == crate::types::MUST_USE)
                .filter_map(|i| crate::types::type_key(&i.ty))
                .collect(),
            types: crate::types::decl_map(program),
        }
    }

    /// Whether a value of `ty` carries a **must-use** obligation, and which row
    /// says so (RFC-0086 M3). `None` for a type that carries none.
    ///
    /// Ownership is affine — under RFC-0089 rule 1 a value you stop using is
    /// simply released, and that alone would have deleted RFC-0075's "a stream
    /// must be consumed" diagnostic the day rule 1 landed. A must-use type is
    /// the opt-in **linear** case: releasing its memory is not the same thing as
    /// discharging it, because its producer has a teardown no memory drop can
    /// run.
    ///
    /// The lookup is [`Owned::release_kind`]'s, one protocol over, and it is
    /// the same two halves in the same order. A **declared** row is read off the
    /// type key, so `impl MustUse for Txn` is what `Txn` means. Otherwise the
    /// type is resolved through its declarations and answered by the one seeded
    /// row, `Stream` — so a `type Events = Stream<Event>` carries the obligation
    /// its base does. The seed is in the compiler rather than in `std/` for the
    /// reason [`crate::types::MUST_USE`] records.
    pub fn linear_kind(&self, ty: &Type) -> Option<Linear> {
        if let Some(k) = crate::types::type_key(ty).filter(|k| self.linear.contains(k)) {
            return Some(Linear::Declared(k));
        }
        // A type that reaches ITSELF has no bottom to a structural walk, and
        // `type Node = { kids: Array<Node> }` is ordinary Vyrn. The guard is
        // [`Owned::release_kind`]'s, for the same reason and in the same words:
        // the answer is `None`, so the obligation is not seen rather than the
        // compiler not returning.
        if self_referring(ty, &self.types).is_some() {
            return None;
        }
        match crate::types::resolve(ty, &self.types) {
            Type::Stream(_) => Some(Linear::Stream),
            // RFC-0095 M1. The same obligation, one type over, for the same
            // reason: a `join` that may run twice cannot free anything, so
            // "free at the last join" needs to know there is only one join —
            // and that is ownership of the `Task` value.
            Type::Task(_) => Some(Linear::Task),
            // RFC-0092 M4: a container answers what its ELEMENT answers. A `Txn`
            // put in an array is still a `Txn`, and reading the obligation off
            // the container's own type key let a program park one in a container
            // and walk away from it. What makes the answer safe to give is M2 and
            // M3: an `Array`, a `SmallArray`, a `Map` and a fixed array release
            // their elements now, so `drop pool` runs the declared `release` per
            // element and the discharge is real rather than nominal.
            //
            // A **record field** is left alone. RFC-0092 says *container*, and a
            // record is a type a program names — `impl MustUse for Order` is how
            // its author says an `Order` holding a `Txn` must be discharged, and
            // is one line where an inferred obligation would be a rule nobody
            // wrote. The corpus agrees: it stores no must-use type in a record.
            //
            // A type PARAMETER answers `None`, which is what keeps every generic
            // container working: `resolve` leaves a `Param` alone and leaves an
            // undeclared `Named` as `Unit`, so `Array<T>` in `map`, `filter`,
            // `fold` and `std/slots` carries no obligation.
            Type::Array(e) | Type::ArrayN(e, _) | Type::SmallArray(e, _) | Type::Option(e) => {
                self.linear_kind(&e)
            }
            Type::Map(a, b) | Type::Result(a, b) => {
                self.linear_kind(&a).or_else(|| self.linear_kind(&b))
            }
            _ => None,
        }
    }

    /// Whether a value of `ty` has to be handed on by name — letting it go out
    /// of scope is an error. [`Owned::linear_kind`] with the row forgotten.
    pub fn must_use(&self, ty: &Type) -> bool {
        self.linear_kind(ty).is_some()
    }

    /// Whether `ty` transitively owns heap, against this program's declarations.
    ///
    /// Not the same question as [`Owned::release_kind`] answering `Some`, and
    /// the gap between the two is every row RFC-0092 M3 adds: a record owns two
    /// Strings and has no release rule. [`Leak::NoRelease`] needs both answers to
    /// word itself.
    pub fn owns_heap(&self, ty: &Type) -> bool {
        owns_heap(ty, &self.types)
    }

    /// The name `ty` reaches from itself with **no declared release between** —
    /// the only self-reference a structural release walk cannot bottom out in.
    ///
    /// [`self_referring`] answers about the TYPE and is what `copy` asks.
    /// This answers about the WALK, and the two differ by one fact: a type that
    /// declared `impl Owned` is released by a CALL, so the walk stops at the
    /// declaration and the cycle behind it is that function's business.
    /// `type Node = { kids: Array<Node> }` has no bottom; the same type with
    /// `impl Owned for Node` written on it has one, and every type that merely
    /// REACHES it gets its structural row back — `{ root: Node, err: String }`
    /// releases both places again.
    ///
    /// It is the whole of RFC-0096: the declaration closes what the walk cannot,
    /// and it closes it for the types above as well as for itself.
    pub fn unbounded(&self, ty: &Type) -> Option<String> {
        self_referring_past(ty, &self.types, &|n| self.impls.contains_key(n))
    }

    /// How a value of `ty` is reclaimed, or `None` for one that owns no heap.
    ///
    /// A **declared** row wins over the seed, so `impl Owned for T` is what `T`
    /// means rather than what `T` happens to be made of. Otherwise the type is
    /// resolved through its declaration — a nominal type over `String` IS a
    /// String — and answered by the seed.
    ///
    /// The match has no `_` arm on purpose. A new [`Type`] variant does not get
    /// to be silently unreclaimed; it has to say so.
    pub fn release_kind(&self, ty: &Type) -> Option<DropKind> {
        if let Some(f) = crate::types::type_key(ty).and_then(|k| self.impls.get(&k)) {
            return Some(DropKind::Release(f.clone(), ty.clone()));
        }
        match crate::types::resolve(ty, &self.types) {
            // ---- the seeded built-in rows ----------------------------------
            Type::Str => Some(DropKind::FreeStr),
            // An `Array<T>` gives back its buffer, and since RFC-0092 M2 the
            // ELEMENTS in it too — census U4. The proof the elements are the
            // array's own is M1's rule: every route into an element is a store,
            // rule 2 refuses storing a borrow, and the rule refuses storing a
            // projection. What was left was the compiler's own back doors, and
            // there were three: `m.keys()`, `sa.toArray()` and `xs.toArray()` on
            // a plain array, which handed the receiver's triple straight back.
            // All three copy now, and so does the synthesized `fromJson`
            // decoder, which is Vyrn the rule never got to check.
            //
            // The recursion is the row. An element is released the way its own
            // type is released, so `Array<Record>` releases nothing until M3
            // gives a record its row, and `Array<Array<String>>` follows the day
            // this one lands. Answering `Deep` rather than a wider `FreeArr` is
            // what routes it through the release WALK, which is `copy` run
            // backwards and already knows every payload encoding.
            //
            // An element type that reaches ITSELF (`type L = Array<L>`) answers
            // the buffer alone. The walk is structural, so a self-referring
            // element has no bottom — the same crash `copy` met in Phase 4b, and
            // the same guard. The elements leak, which is what this whole file
            // does where it cannot prove otherwise.
            //
            // Unless the cycle has a DECLARATION on it (RFC-0096). The guard is
            // [`Owned::unbounded`] rather than [`self_referring`]: a declared
            // release is a call, so the walk stops there and `Array<Node>` gets
            // its element row back the day `impl Owned for Node` is written.
            Type::Array(e) => Some(
                if self.unbounded(&e).is_none() && self.release_kind(&e).is_some() {
                    DropKind::Deep(Type::Array(e))
                } else {
                    DropKind::FreeArr
                },
            ),
            // The same recursion, over the two containers with no view
            // constructor between them and the rule (RFC-0092 M3). A
            // `SmallArray` releases its inline and spilled slots; a `Map`
            // releases its keys, which are always `String`, and its values where
            // the value type has a row.
            Type::SmallArray(e, n) => {
                let t = Type::SmallArray(e.clone(), n);
                Some(
                    if self.unbounded(&t).is_none() && self.release_kind(&e).is_some() {
                        DropKind::Deep(t)
                    } else {
                        DropKind::FreeSmallArr
                    },
                )
            }
            Type::Map(k, v) => {
                let t = Type::Map(k.clone(), v.clone());
                Some(
                    if self.unbounded(&t).is_none()
                        && (self.release_kind(&k).is_some() || self.release_kind(&v).is_some())
                    {
                        DropKind::Deep(t)
                    } else {
                        DropKind::FreeMap
                    },
                )
            }
            // A `Stream<T>` is reclaimed too, but through the stream lowering
            // (RFC-0075 M2b), which pushes its own release frame at the binding
            // that produces it. Answering here as well would release it twice.
            //
            // A `Task<T>` is the same shape since RFC-0095 M1, and the same
            // answer for the same reason. A task is reclaimed by the construct
            // that DISCHARGES it — `t.join()` takes the result and frees, `drop
            // t` waits, releases the result by its type and frees — and an
            // automatic block-exit row would free it a second time. Both
            // constructs need the frame pointer and the result's type, neither
            // of which this table carries, so both lowerings emit it directly.
            Type::Stream(_) | Type::Task(_) => None,
            // ---- everything the language stores by value --------------------
            Type::Int
            | Type::IntN { .. }
            | Type::Float
            | Type::Float32
            | Type::F32x4
            | Type::I32x4
            | Type::F64x2
            | Type::Mask32x4
            | Type::Mask64x2
            | Type::Bool
            | Type::Unit
            | Type::ConstInt(_)
            | Type::Logger
            | Type::Never
            | Type::Err => None,
            // ---- aggregates that own their places (Phase 5) -----------------
            // Until rule 2 landed, each of these was a value the engines copy
            // whose heap contents belonged to whoever produced them, so a row
            // here would have freed a payload the producer still held. Rule 2
            // moved the answer: a store into a place is a move, a struct literal
            // and a variant constructor ARE stores, and a borrow may not be
            // stored at all. So an aggregate holds what it holds, and rule 4
            // says releasing it releases those places.
            //
            // Two rows, and only two. Census §14 is the whole reason: `Option`
            // and `Result` are how this language is told to write a fallible
            // function, so a String built inside one had no owner in the
            // RECOMMENDED style.
            t @ (Type::Option(_) | Type::Result(..)) => {
                owns_heap(&t, &self.types).then(|| DropKind::Deep(t))
            }
            // A stored function value (RFC-0037) is `{ tag, captures }` and the
            // capture block IS heap — one `malloc` per evaluation of the lambda,
            // which is census §16. Phase 10b releases it: the block is one
            // allocation whatever the tag, so the release is the same three
            // instructions the stream closer already emits, and the walk reaches
            // it through `Deep` like any other place.
            //
            // The release is SHALLOW — the block, not what the captures point
            // at. Two lambdas over one String build two blocks holding one
            // pointer, so a deep release would free it twice. A captured String
            // therefore still leaks, and `Gone::Captured` already says why
            // nothing else releases it either.
            t @ Type::Fn(..) => Some(DropKind::Deep(t)),
            // A **record**, a **user enum** and a fixed **`[N x T]`** release
            // their places since RFC-0092 M3 — RFC-0089 rule 4 in the words the
            // rule uses, and the half of it that had been open since Phase 5.
            //
            // Phase 5 measured why it could not land then. All three hand their
            // insides out as PROJECTIONS, and rule 3 recorded a returned
            // projection as a LEND rather than refusing it, so three parity runs
            // failed within a minute of each other: `std/jsondec`'s `tagOf(v)`
            // handed back a `String` its `Json` still held, `std/graphql`'s
            // `gqlScanner(src)` returned a record holding a view of its
            // argument, and `gqlParseQuery` wrote `GqlQuery { sels: set.sels }`
            // — a field read stored into a literal.
            //
            // **M1 refuses all three spellings.** A projection of a place the
            // frame owns may not be stored and may not be returned, and the
            // corpus took 116 `.copy()` calls to say so. So a record holds what
            // it holds, and releasing it releases those places.
            //
            // The guard is the one the `Array` row carries: a type that reaches
            // ITSELF has no bottom to a structural walk (`type Node = { kids:
            // Array<Node> }` is ordinary Vyrn). It answers `None` and its places
            // leak, which is the answer this file gives wherever it cannot prove
            // otherwise. `Json` and `Html` are that shape, which is also why M1's
            // `.copy()` menu sent them to a hand-written `copyJson`.
            //
            // RFC-0096 moved the guard one word: a cycle with `impl Owned` on it
            // is bounded, because the walk emits a CALL at the declaration. So a
            // record holding a declared self-referring type is walked again, and
            // 63 corpus bindings closed on two declarations rather than eleven.
            //
            // A `Fn` is off for the reason `owns_heap` records, and `lazy T` IS
            // `fn() -> T` (RFC-0085 M4a) — `resolve` normally answers that, and
            // this is the depth-limited fallback.
            t @ (Type::Record(_) | Type::Enum(_) | Type::ArrayN(..)) => {
                (self.unbounded(ty).is_none() && owns_heap(&t, &self.types))
                    .then(|| DropKind::Deep(t))
            }
            Type::Lazy(_) => None,
            // ---- shapes that are not a runtime value ------------------------
            // A type operator survives only until `resolve` reaches its base, a
            // `Param` is erased by monomorphization, and an unresolved `Named`
            // or `App` is a name with no declaration. None of them reaches a
            // binding whose cleanup this decides.
            Type::Omit(..)
            | Type::Pick(..)
            | Type::Merge(..)
            | Type::Partial(_)
            | Type::Param(_)
            | Type::Named(_)
            | Type::App(..) => None,
        }
    }
}

/// The name of a type `ty` reaches from itself, if it reaches one.
///
/// `copy` is structural and recursive (RFC-0089 M1b), and a structural walk of a
/// type that refers to itself has no bottom. Both compiling backends expanded
/// one until the process ran out of stack — a crash, at compile time, with no
/// diagnostic and no line. Phase 4b found it because rule 3 sends a `Json` field
/// lookup through `copy`, and this predicate is what turns the crash into a
/// refusal that names the type.
///
/// The answer for a self-referring type is a function: recursion in the value
/// needs recursion in the code, and `std/json`'s `copyJson` is the worked
/// example. RFC-0091 M1's `Copy` protocol is where a type declares its own.
pub fn self_referring(ty: &Type, types: &HashMap<String, TypeDecl>) -> Option<String> {
    self_referring_past(ty, types, &|_| false)
}

/// [`self_referring`], with the names a walk STOPS at removed from the question.
///
/// One walk, two readers. `copy` stops at nothing, so it passes a predicate that
/// is never true and reads the type's own shape. A release stops at every type
/// that declared `impl Owned`, because the walk emits a CALL there rather than
/// expanding — see [`Owned::unbounded`].
fn self_referring_past(
    ty: &Type,
    types: &HashMap<String, TypeDecl>,
    stops: &dyn Fn(&str) -> bool,
) -> Option<String> {
    fn go(
        ty: &Type,
        types: &HashMap<String, TypeDecl>,
        stops: &dyn Fn(&str) -> bool,
        seen: &mut Vec<String>,
    ) -> Option<String> {
        if let Type::Named(n) | Type::App(n, _) = ty {
            // The walk ends here, so nothing behind this name is on it.
            if stops(n) {
                return None;
            }
            if seen.iter().any(|s| s == n) {
                return Some(n.clone());
            }
            // Not a declared name: nothing to expand, so nothing to recur into.
            if !types.contains_key(n) {
                return None;
            }
            seen.push(n.clone());
            let r = go(&crate::types::resolve(ty, types), types, stops, seen);
            seen.pop();
            return r;
        }
        let mut deeper = |t: &Type| go(t, types, stops, seen);
        match ty {
            Type::Option(t)
            | Type::Array(t)
            | Type::ArrayN(t, _)
            | Type::SmallArray(t, _)
            | Type::Lazy(t)
            | Type::Task(t)
            | Type::Stream(t) => deeper(t),
            Type::Result(a, b) | Type::Map(a, b) => deeper(a).or_else(|| deeper(b)),
            Type::Record(fs) => fs.iter().find_map(|f| go(&f.ty, types, stops, seen)),
            Type::Enum(vs) => vs
                .iter()
                .find_map(|v| v.payload.iter().find_map(|p| go(p, types, stops, seen))),
            _ => None,
        }
    }
    go(ty, types, stops, &mut Vec::new())
}

/// Whether a value of `ty` transitively owns heap, under RFC-0089 rule 1.
///
/// [`Owned::release_kind`] answers about the value's OWN storage; this asks
/// about everything it reaches, because a record of Strings moves under rule 1
/// even though releasing the record releases nothing today.
///
/// The depth limit is the same guard the rest of this file uses against a
/// declaration that refers to itself; a type that deep is answered `false`, which
/// costs a copy that copies nothing and never a wrong free.
pub fn owns_heap(ty: &Type, types: &HashMap<String, TypeDecl>) -> bool {
    fn go(ty: &Type, types: &HashMap<String, TypeDecl>, seen: &mut Vec<String>) -> bool {
        // A NAME THAT REACHES ITSELF OWNS HEAP, and answering otherwise is what
        // this function used to do. The guard was `if depth > 8 { false }`, so
        // `type Tree = | Leaf | Node(Tree, Tree)` — which is nothing but heap —
        // exhausted the counter and answered "no". Two things followed from that
        // one word:
        //
        //   - `vyrn why --memory` reported "the return type Tree owns no heap".
        //   - `Gen::release_enum` skips a variant whose payloads own nothing, so
        //     RFC-0096's `free_declared_boxes` freed nothing for exactly the
        //     variant whose boxes needed it. 200,000 trees of depth 8, built and
        //     discarded one at a time, peaked at 3.1 GB with a live set of one.
        //
        // The cycle is the answer, not the limit. A type that reaches itself
        // cannot be stored inline — the representation has to box the recursive
        // field to be finite — and that box is heap whatever else the type
        // holds. So a repeated name answers `true` where the counter answered
        // `false`.
        //
        // `seen` keyed on the NAME, which is `self_referring_past`'s shape a few
        // functions up: the two ask different questions about the same walk, and
        // now they walk the same way.
        if let Type::Named(n) | Type::App(n, _) = ty {
            if seen.iter().any(|x| x == n) {
                return true;
            }
            if !types.contains_key(n) {
                return false;
            }
            seen.push(n.clone());
            let r = go(&crate::types::resolve(ty, types), types, seen);
            seen.pop();
            return r;
        }
        let deeper = |t: &Type| go(t, types, &mut seen.clone());
        match crate::types::resolve(ty, types) {
            // A `Task<T>` owns a frame, a record and an operating-system handle
            // whatever `T` is (RFC-0095 M1), so it answers `true` for `Task<Unit>`
            // as much as for `Task<String>`. It answered `deeper(T)` until M1,
            // which is what let a `Task<Int64>` be copied, stored and abandoned
            // as if it were a number.
            Type::Str
            | Type::Array(_)
            | Type::SmallArray(..)
            | Type::Map(..)
            | Type::Stream(_)
            | Type::Task(_) => true,
            Type::Option(t) | Type::ArrayN(t, _) | Type::Lazy(t) => deeper(&t),
            Type::Result(a, b) => deeper(&a) || deeper(&b),
            Type::Record(fs) => fs.iter().any(|f| deeper(&f.ty)),
            Type::Enum(vs) => vs.iter().any(|v| v.payload.iter().any(&deeper)),
            // A stored function value (RFC-0037) is `{ tag, captures }` and the
            // capture block IS heap — one `malloc` per evaluation of the lambda,
            // which is census §16.
            //
            // It answered `false` until Phase 10b, and the price of the honest
            // answer is what held it there. `true` makes a `fn` move under rule
            // 1, which is the only thing that lets rule 4 release it: a value
            // that copies freely is two names for one block and the release runs
            // twice. The corpus stores them — `std/http` hands `run`, `whole`
            // and `feed` straight across into a new record and `std/ui` does the
            // same for `Query.run` — so each of those became a store of a
            // borrowed `fn`, and the fix menu's second entry, `.copy()`, had
            // nothing to lower to: a capture block's layout is per TAG, chosen
            // at run time, so a structural copy has nothing to measure.
            //
            // **RFC-0091 M1 was named as the mechanism and is not it.** M1 keys
            // a `Copy` row by a TYPE KEY. A `fn` type is structural and has
            // none, and a `type Bump = fn(..) -> ..` alias over one is refused
            // where it is written: the value erases at run time and carries no
            // name to dispatch on. So §16 has nowhere to hang a declaration, and
            // nothing to write in it either — the tags are the
            // defunctionalizer's and have no source name.
            //
            // Phase 10b derived the copy over the defunctionalized enum instead,
            // where RFC-0037 emits that enum and knows every tag's layout
            // because it chose them. `@__vyrn_fnval_copy` is one function per
            // module: a switch from tag to block size, then one `malloc` and one
            // `memcpy`. The corpus price came out at 22 sites rather than the
            // "the corpus copies them" this comment predicted — 17 take
            // `consume` and 5 take `.copy()`, and the 5 are the ones whose
            // source is `self.feed`, where an impl receiver cannot be declared
            // `consume` at all.
            Type::Fn(..) => true,
            _ => false,
        }
    }
    go(ty, types, &mut Vec::new())
}

/// Why a binding is **not** reclaimed at block exit (RFC-0087 U1).
///
/// Three of the rows come straight from [`crate::movecheck`], which decided them
/// while it was enforcing rules 1 to 3. The other two are this file's, because
/// neither is about the value: an arena owns what is allocated inside it, and a
/// literal was never allocated.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Leak {
    /// The type releases nothing. Carries the type, or `unknown`, and whether
    /// that type owns heap.
    ///
    /// One `None` from `release_kind` mints two different facts, and calling
    /// them one thing made the printer contradict itself: `vyrn why --memory`
    /// said "the type `Doc` owns no heap" about a record holding two Strings,
    /// one line under "the caller owns the result". A scalar has nothing to
    /// reclaim and is not a leak; a type that owns heap and has no release row
    /// is the leak this arc is closing (RFC-0092 M0).
    NoRelease { ty: String, owns_heap: bool },
    /// The binding names storage somebody else owns (rule 2). Carries what it
    /// is, in words.
    Borrowed(&'static str),
    /// Lexically inside a `region` — the arena owns it.
    Region,
    /// A lambda or a `spawn` holds it, and either can outlive this block.
    Captured { line: usize },
    /// A second name reads it without taking it, so neither name is the owner.
    Aliased { line: usize },
    /// It reached a call that may retain it.
    Escaped { callee: String, line: usize },
    /// A `consume` took one of its places (RFC-0093 M1) and the walk may not be
    /// told to skip it, so releasing the binding would free what the take gave
    /// away. Carries the places taken.
    ///
    /// RFC-0093 M2 releases the rest of the value wherever the walk CAN skip
    /// them, which is [`Fate::Reclaimed`]'s second field. Three cases stay here,
    /// and each of them is a place the walk cannot be told about: a declared
    /// `release`, which is a user function; a path that is not a chain of record
    /// fields, because an enum's live variant is a runtime tag; and a hole a
    /// later write filled, because the store that filled it already released
    /// what the take gave away.
    Hole { paths: Vec<String>, line: usize },
}

impl Leak {
    /// The reason with its lines and names removed, so a corpus of them groups.
    pub fn kind(&self) -> &'static str {
        match self {
            Leak::NoRelease {
                owns_heap: false, ..
            } => "the type owns no heap",
            Leak::NoRelease {
                owns_heap: true, ..
            } => "the type has no release rule",
            Leak::Borrowed(_) => "it names somebody else's value",
            Leak::Region => "inside a `region`",
            Leak::Captured { .. } => "captured by a lambda or a spawn",
            Leak::Aliased { .. } => "aliased by another binding",
            Leak::Escaped { .. } => "escaped into a call",
            Leak::Hole { .. } => "a `consume` took one of its places",
        }
    }
}

impl std::fmt::Display for Leak {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Leak::NoRelease {
                ty,
                owns_heap: false,
            } => write!(f, "the type {ty} owns no heap"),
            Leak::NoRelease {
                ty,
                owns_heap: true,
            } => write!(f, "nothing releases the type {ty} yet"),
            Leak::Borrowed(what) => write!(f, "it is {what}"),
            Leak::Region => write!(f, "it is inside a `region` — the arena owns it"),
            Leak::Captured { line } => {
                write!(f, "a lambda or a spawn captures it at line {line}")
            }
            Leak::Aliased { line } => write!(f, "another binding aliases it at line {line}"),
            Leak::Escaped { callee, line } => {
                write!(f, "it escapes into the call to `{callee}` at line {line}")
            }
            Leak::Hole { paths, line } => write!(
                f,
                "a `consume` took {} at line {line}, so it has a hole in it",
                places(paths)
            ),
        }
    }
}

/// The holes that live INSIDE the field `name`, with the field's own hop
/// removed — what a release walk carries one level down (RFC-0093 M2).
///
/// One copy for both backends. `hs.head.err` reaches the walk of `head` as
/// `err`, and a hole naming a sibling field reaches it as nothing.
pub fn holes_under(holes: &[String], name: &str) -> Vec<String> {
    holes
        .iter()
        .filter_map(|h| h.strip_prefix(name)?.strip_prefix('.'))
        .map(str::to_string)
        .collect()
}

/// Whether `e` ALLOCATED the String it answers — a fresh buffer no binding
/// names, so the expression that consumes it is its only owner (RFC-0096 M3).
///
/// Three forms build one, and every one of them copies out of its operands
/// rather than borrowing them: `@str` renders a value into a fresh buffer,
/// `@concat` and a String `+` build a fresh buffer out of both halves. So an
/// operand this answers `true` for is the concatenation's to release once it
/// has copied — `"n" + i.toString()` leaked the `@str` result at every turn of
/// a loop, and nothing else could ever free it because no binding names it.
///
/// **A caller must also check that the value's type is `String`.** `+` is the
/// one operator that allocates and it is also integer addition and `Code`
/// concatenation; the type is what tells the three apart, and only a backend
/// knows it. `@str` and `@concat` need no such check — the lexer cannot produce
/// a leading `@`, so no user declaration can shadow either name and turn the
/// call into a dispatch that keeps what it is given. That is the same argument
/// `ban_append_expr` already stands on, in the same two names.
///
/// The other way an expression makes a temporary — a CALL's result, handed to
/// another call or to a `+` — is not here and does not belong here. This is a
/// question about the expression's SHAPE, and a call's answer is its callee's:
/// the position may retain the value, or take it, or hand it back. That verdict
/// is [`crate::movecheck::ArgVerdict`], read per position and closed over the
/// call graph, and [`Ownership::arg_drops`] is what the backends ask. A String
/// `+` records its operands there under the name `@concat`, so an operand a
/// call produced takes the same rule and the same guards as a call argument
/// (`rfcs/census-call-arguments.md` §9, finding 3) — and the two rules
/// partition, because an operand this predicate answers `true` for reads
/// `AlreadyFreed` there.
pub fn str_temporary(e: &Expr) -> bool {
    match e {
        Expr::Call { name, .. } => name == "@str" || name == "@concat",
        Expr::Binary { op: BinOp::Add, .. } => true,
        _ => false,
    }
}

/// The places a hole names, in the words both surfaces print.
fn places(paths: &[String]) -> String {
    paths
        .iter()
        .map(|p| format!("`{p}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// What happens to a `let` binding's value at the end of its block.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Fate {
    /// The engines release it here, this way — MINUS the places a `consume`
    /// took out of it (RFC-0093 M2), which are relative to the binding
    /// (`title`, `head.err`) and empty for every binding nothing took from.
    ///
    /// The set rides with the verdict rather than with the [`DropKind`], because
    /// a kind is a property of the TYPE and a hole is a property of one binding:
    /// two records of one type, one drained and one whole, release differently.
    Reclaimed(DropKind, Vec<String>),
    /// It left: a `return` carried it out, or a store took it. Whoever holds it
    /// now reclaims it, so this block must not.
    Moved { line: usize, into: String },
    /// `drop name` reclaims it, so the automatic path must not.
    Dropped { line: usize },
    /// It is static data in the module's data segment. Nothing reclaims it,
    /// and nothing needs to (census §1).
    Static,
    /// It carries a must-use obligation, and the construct that DISCHARGES it
    /// reclaims it (`rfcs/census-regions.md`, defect 2). Carries the row.
    ///
    /// [`Owned::release_kind`] answers `None` for a `Stream<T>` and a `Task<T>`
    /// on purpose — an automatic block-exit row would free each a second time —
    /// and this analysis read that `None` as "nothing reclaims it". The report
    /// then said "nothing releases the type `Task<Int64>` yet" about a task
    /// `examples/concurrency.vyrn` joins on the next line, which is 21 of the
    /// bindings the corpus census flagged.
    ///
    /// The claim is CATEGORICAL rather than per binding. A must-use value that
    /// is not discharged on every path is a compile error (RFC-0075 M1 for a
    /// stream, RFC-0095 M1 for a task), so a program that reaches this analysis
    /// has already been proved to discharge every one of them.
    Discharged(Linear),
    /// Nothing reclaims it.
    Leaked(Leak),
}

impl Fate {
    /// What happens to the value, in one line.
    ///
    /// The wording `vyrn why --memory` has printed since Phase 1, lifted here so
    /// the editor says the same thing (RFC-0087 U1). Nothing re-derives it.
    pub fn words(&self) -> String {
        match self {
            Fate::Reclaimed(kind, holes) if holes.is_empty() => {
                format!("reclaimed at block exit — {}", kind.words())
            }
            Fate::Reclaimed(kind, holes) => format!(
                "reclaimed at block exit — {}, except {}, which a `consume` took",
                kind.words(),
                places(holes)
            ),
            Fate::Moved { line, into } => format!("moved at line {line} into {into}"),
            Fate::Dropped { line } => format!("reclaimed by `drop` at line {line}"),
            Fate::Static => "static data — nothing reclaims it, and nothing needs to".into(),
            // The three menus are the ones `movecheck` prints, one tense over.
            // There the reader is told what to write; here the reader is told
            // what the program already wrote, so the sentence names the same
            // three discharges and says which lowering does the freeing.
            Fate::Discharged(Linear::Stream) => "discharged, not leaked — a stream is consumed, \
                 forwarded or closed on every path, and that lowering frees it"
                .into(),
            Fate::Discharged(Linear::Task) => "discharged, not leaked — a task is joined, \
                 forwarded or dropped on every path, and that lowering frees it"
                .into(),
            Fate::Discharged(Linear::Declared(by)) => format!(
                "discharged, not leaked — `{by}` declares `impl MustUse`, so it is handed on \
                 or dropped on every path"
            ),
            Fate::Leaked(reason) => format!("NOT reclaimed — {reason}"),
        }
    }

    /// The line where the value stops being live, when there is one.
    ///
    /// A move and a `drop` are points in the source; block-exit reclamation, a
    /// literal and a leak are not. This is what the editor marks as the last use
    /// (RFC-0087 U1) and what it writes an inlay hint beside.
    pub fn last_use(&self) -> Option<usize> {
        match self {
            Fate::Moved { line, .. } | Fate::Dropped { line } => Some(*line),
            _ => None,
        }
    }
}

/// One `let` binding and what happens to its value — the report behind
/// `vyrn why --memory`.
#[derive(Clone, Debug)]
pub struct BindingNote {
    pub name: String,
    pub line: usize,
    pub fate: Fate,
}

/// Whole-program ownership facts.
#[derive(Default)]
pub struct Ownership {
    /// Functions whose return value transfers heap ownership to the caller,
    /// with the kind of value returned.
    ///
    /// Since RFC-0089 rule 3 this is the return type and nothing else: a return
    /// is owned, and `movecheck` refuses the program where it is not. The
    /// fixpoint that used to compute it asked a question the language now
    /// answers.
    pub owned_fns: HashMap<String, DropKind>,
    /// Per function: identity of each droppable `let` and how to reclaim it.
    pub droppable: HashMap<String, HashMap<usize, DropKind>>,
    /// Per function: the places a `consume` took out of a droppable `let`
    /// (RFC-0093 M2), keyed the same way and relative to the binding. A `let`
    /// with no row here has no hole, which is nearly all of them.
    ///
    /// A second map rather than a field on [`DropKind`]: the kind answers for a
    /// TYPE and every construction site of it would have to carry an empty set,
    /// including the ones that answer for a store or for an explicit `drop`.
    pub holes: HashMap<String, HashMap<usize, Vec<String>>>,
    /// Per function: every `let` in source order, and what happens to its value
    /// — the same decisions `droppable` carries, plus the reason for each one
    /// this analysis did NOT take. Recorded by the walker that decides, so the
    /// report and the emission cannot disagree (RFC-0087 U1).
    pub notes: HashMap<String, Vec<BindingNote>>,
    /// The `Owned` table this analysis decided with. Handed out so a backend
    /// lowering an explicit `drop x` asks the SAME question the automatic path
    /// asked, instead of keeping a second copy of the answer.
    pub proto: Owned,
    /// Every call-argument temporary in the program, with the callee's verdict
    /// on it (`rfcs/census-call-arguments.md`). The rows a backend acts on are
    /// the [`crate::movecheck::ArgVerdict::Released`] ones, which is what
    /// [`Ownership::arg_drops`] hands back; the rest are here so the census's
    /// own table can be re-derived from the compiler instead of from a harness.
    pub arg_temps: Vec<crate::movecheck::ArgTemp>,
    /// Per function: [`droppable`](Ownership::droppable)'s rows PLACED — every
    /// step, at the exit that runs it, in the order it runs (RFC-0101 M4).
    ///
    /// Grouped by [`Release::site`], which is the node the exit is at. This is
    /// the one order that used to be asserted separately by `Gen::drop_stack`,
    /// `Fn_::releases` and the interpreter's per-block `Vec`.
    pub releases: HashMap<String, Vec<Release>>,
}

impl Ownership {
    /// Every argument temporary the CALLER releases after the call, keyed by the
    /// argument expression's node address — the key a backend releases by, the
    /// way `droppable` is keyed by the `Stmt::Let`'s.
    ///
    /// Only a `String` today. Both backends free one out of a register with a
    /// helper each already has ([`str_temporary`]'s), and every other kind wants
    /// the walking release, which needs a PLACE and therefore a slot to spill
    /// into. The rows for those kinds are recorded and left alone: a leak, which
    /// is what they are today.
    ///
    /// **A wider kind has one more question to ask first.** A seeded row may
    /// hand a CONTAINER back — `@push` answers `Array<T>` and its receiver is
    /// the same `Array<T>` — so freeing at the call would free a buffer the
    /// result still names. No `Array` is a `FreeStr`, so no such row can reach
    /// this filter; `movecheck::arg_verdict` answers the same question for a row
    /// that hands back a bare type parameter, which `blackBox` does.
    pub fn arg_drops(&self) -> std::collections::HashSet<usize> {
        // EVERY `Released` temporary, whatever its kind (RFC-0114 M1). This
        // carried `&& s.kind == DropKind::FreeStr` for as long as the emitters
        // could only free a String — so a call-argument tree, array or record
        // was analysed, verdicted, and then thrown away here, which is why
        // `check(make(depth))` leaked 313.9 MB against the interpreter's 8.5.
        // Both backends free by TYPE now, through the same `release_kind`
        // table this filter used to consult, so the filter is the leak.
        self.arg_temps
            .iter()
            .filter(|s| s.verdict == crate::movecheck::ArgVerdict::Released)
            .map(|s| s.id)
            .collect()
    }
}

/// Analyse ownership across a whole program.
pub fn analyze(program: &Program) -> Ownership {
    let proto = Owned::new(program);
    // What every `let` in the program still owns where its block ends, decided
    // by the pass that enforces the rules. One walk, one answer, no second
    // opinion (RFC-0087 records three defects that were two walkers disagreeing).
    let facts = crate::movecheck::facts(program);
    let lets = facts.lets;

    let mut droppable = HashMap::new();
    let mut holes = HashMap::new();
    let mut notes = HashMap::new();
    let mut releases = HashMap::new();
    let mut emit = |name: String, body: &Block| {
        let r = emit_body(body, &lets, &proto);
        releases.insert(name.clone(), place_body(body, &r.droppable));
        droppable.insert(name.clone(), r.droppable);
        holes.insert(name.clone(), r.holes);
        notes.insert(name, r.notes);
    };
    for f in &program.functions {
        emit(f.name.clone(), &f.body);
    }
    // Test bodies (RFC-0015) get the same block-exit drops, so a `let` in a test
    // reclaims its heap value exactly as it would in a function. The body is the
    // REAL node the interpreter walks, so the by-address keys match at run time.
    for (i, t) in program.tests.iter().enumerate() {
        emit(format!("test@{i}"), &t.body);
    }
    // Bench bodies (RFC-0055), keyed by the synthetic `bench@<index>` name the
    // interpreter (`--check`) walks.
    for (i, b) in program.benches.iter().enumerate() {
        emit(format!("bench@{i}"), &b.body);
    }
    // Rule 3: a return is owned. The return type is the whole answer.
    let owned_fns = program
        .functions
        .iter()
        .filter_map(|f| proto.release_kind(&f.ret).map(|k| (f.name.clone(), k)))
        .collect();
    Ownership {
        owned_fns,
        droppable,
        holes,
        notes,
        proto,
        arg_temps: facts.arg_temps,
        releases,
    }
}

/// The placement as a consumer reads it: `(exit, the node the exit is AT)` maps
/// to the bindings released there, in the order they run.
///
/// One reader for three engines. An engine standing at an exit has the node in
/// hand, looks its steps up, and maps each binding to whatever it releases a
/// value WITH — an alloca name, a wasm place, a scope entry. What it never does
/// again is decide the order, or derive a boundary index to find where its own
/// frames stop.
pub fn placed(steps: &[Release]) -> HashMap<(Exit, usize), Vec<usize>> {
    let mut out: HashMap<(Exit, usize), Vec<usize>> = HashMap::new();
    for r in steps {
        out.entry((r.exit, r.site)).or_default().push(r.binding);
    }
    out
}

// ---- the placement --------------------------------------------------------
//
// RFC-0101 M4. `emit_body` above answers WHETHER a binding is droppable and
// nominally HOW; this answers WHERE and IN WHAT ORDER, once, for every engine.
//
// It is a second walk over the same body rather than a field on `Emit`, because
// the two ask different questions of different shapes: `Emit` is a dataflow over
// `movecheck`'s facts and this is a stack discipline over source order. Fusing
// them would put a frame stack inside a fixpoint for no reader's benefit.

/// A value on a live frame: what [`Emit`] said about it, before an exit says
/// where.
struct Live {
    binding: usize,
    name: String,
    kind: DropKind,
    line: u32,
}

/// The live frames and the loop boundaries — the one model of what
/// `Gen::drop_stack`, `Fn_::releases` and the interpreter's per-block `Vec` each
/// used to keep privately.
struct Place<'a> {
    droppable: &'a HashMap<usize, DropKind>,
    out: Vec<Release>,
    /// Innermost last. An exit's steps are these frames, from a boundary
    /// outward.
    frames: Vec<Vec<Live>>,
    /// One entry per enclosing loop: the frame index its body starts at, which
    /// is where `break` and `continue` unwind to. Below it sits the frame a
    /// `for`-in's iterable is on, which is why neither edge reaches that one.
    loops: Vec<usize>,
}

/// Place one body's steps.
///
/// **It walks the program's own nodes and no expansion.** A `place at`
/// projection and a `for` over a user container are inlined at their access site
/// (RFC-0101 M2d), and the inline is a CLONE: no node in it is a key of
/// `droppable`, so no step can be placed inside one. Skipping them is what lets
/// the placement live here, in the crate the interpreter can reach, instead of
/// needing the checker's recorded types to expand a projection.
fn place_body(body: &Block, droppable: &HashMap<usize, DropKind>) -> Vec<Release> {
    let mut p = Place {
        droppable,
        out: Vec::new(),
        frames: Vec::new(),
        loops: Vec::new(),
    };
    p.block(body);
    p.out
}

impl Place<'_> {
    /// Put a value this map calls droppable on the innermost live frame.
    ///
    /// `key` is `own`'s own key: the `Stmt::Let` for a binding, the construct
    /// itself for the temporary it owns. A value with no row is not on a frame.
    fn track(&mut self, key: usize, name: &str, line: u32) {
        let Some(kind) = self.droppable.get(&key) else {
            return;
        };
        if let Some(f) = self.frames.last_mut() {
            f.push(Live {
                binding: key,
                name: name.to_string(),
                line,
                kind: kind.clone(),
            });
        }
    }

    /// Place the steps one exit runs: every frame from `from` outward, innermost
    /// frame first and newest binding first inside each.
    ///
    /// That order is the whole of what three engines used to assert separately.
    /// It is derived here from source order and this map, and nowhere else.
    fn place(&mut self, from: usize, exit: Exit, site: usize) {
        let steps: Vec<Release> = self.frames[from..]
            .iter()
            .rev()
            .flat_map(|f| f.iter().rev())
            .map(|l| Release {
                site,
                binding: l.binding,
                name: l.name.clone(),
                kind: l.kind.clone(),
                exit,
                line: l.line,
            })
            .collect();
        self.out.extend(steps);
    }

    fn block(&mut self, b: &Block) {
        self.frames.push(Vec::new());
        let here = self.frames.len() - 1;
        for s in &b.stmts {
            self.stmt(s);
        }
        // After the statements, so a nested block's exit steps precede its
        // parent's — "innermost frame first" written as the order they are in.
        self.place(here, Exit::Block, b as *const Block as usize);
        self.frames.pop();
    }

    /// A construct that owns a TEMPORARY runs its body inside a frame of its own
    /// and releases it when the construct is done — the shape `Stmt::IfLet` has
    /// had since Phase 10a, `Stmt::ForIn` since RFC-0092 M5 and `Expr::Match`
    /// since `movecheck` gave a match's scrutinee a row.
    ///
    /// The frame is pushed AFTER the scrutinee is walked and BEFORE any loop
    /// boundary, which is what both compiled backends do and what makes the two
    /// facts true: an early exit out of an arm reclaims it, and a `break` does
    /// not.
    fn owned_temp(&mut self, key: usize, name: &str, line: u32, body: impl FnOnce(&mut Self)) {
        self.frames.push(Vec::new());
        let here = self.frames.len() - 1;
        // The construct's own word, because the value has no name to print: it
        // is a temporary, which is why `own` keys its row by the construct.
        self.track(key, name, line);
        body(self);
        self.place(here, Exit::Scrutinee, key);
        self.frames.pop();
    }

    fn stmt(&mut self, s: &Stmt) {
        let line = stmt_line(s) as u32;
        match s {
            Stmt::Let { name, value, .. } => {
                self.expr(value);
                // On the frame AFTER its value is walked, because the value may
                // itself leave the function — `let a = f()?` reclaims what was
                // live before `a`, and `a` is not one of them.
                self.track(id(s), name, line);
            }
            Stmt::Assign { value, .. } | Stmt::SetField { value, .. } => self.expr(value),
            Stmt::IndexSet { index, value, .. } => {
                self.expr(index);
                self.expr(value);
            }
            // A function exit: every frame the body has open, innermost first.
            // The value is walked first because it runs first, and because it
            // may hold a `?` of its own — a function exit from further in.
            Stmt::Return { value, .. } => {
                if let Some(v) = value {
                    self.expr(v);
                }
                self.place(0, Exit::Return, id(s));
            }
            // The loop edges: every frame the innermost loop's body opened, and
            // no more.
            Stmt::Break { .. } => {
                self.place(self.loops.last().copied().unwrap_or(0), Exit::Break, id(s))
            }
            Stmt::Continue { .. } => self.place(
                self.loops.last().copied().unwrap_or(0),
                Exit::Continue,
                id(s),
            ),
            Stmt::Drop { .. } => {}
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                self.expr(cond);
                self.block(then_block);
                if let Some(e) = else_block {
                    self.block(e);
                }
            }
            Stmt::IfLet {
                scrutinee,
                then_block,
                else_block,
                ..
            } => {
                self.expr(scrutinee);
                self.owned_temp(id(s), "@iflet", line, |p| {
                    p.block(then_block);
                    if let Some(e) = else_block {
                        p.block(e);
                    }
                });
            }
            Stmt::While { cond, body, .. } => {
                self.expr(cond);
                self.loops.push(self.frames.len());
                self.block(body);
                self.loops.pop();
            }
            Stmt::ForIn { iter, body, .. } => {
                self.expr(iter);
                self.owned_temp(id(s), "@forin", line, |p| {
                    // The boundary sits ABOVE the iterable's frame, so `break`
                    // and `continue` leave the snapshot alone and land on the
                    // code that releases it at the statement's own exit.
                    p.loops.push(p.frames.len());
                    p.block(body);
                    p.loops.pop();
                });
            }
            Stmt::Expr(e) => self.expr(e),
            Stmt::Region { body, .. } => self.block(body),
        }
    }

    fn expr(&mut self, e: &Expr) {
        match e {
            Expr::Int(_)
            | Expr::Byte(_)
            | Expr::Float(_)
            | Expr::Bool(_)
            | Expr::Str(_)
            | Expr::Var { .. } => {}
            Expr::Unary { expr: inner, .. } | Expr::Field { expr: inner, .. } => self.expr(inner),
            // A propagating `?` is a function exit and pays what one pays — the
            // sentence RFC-0101 M4's step 0 wrote nine lines of interpreter for.
            // The steps are placed unconditionally: an engine reaches them only
            // on the failing branch, which is a target fact about where the code
            // goes rather than a decision about what runs.
            Expr::Try { expr: inner, .. } => {
                self.expr(inner);
                self.place(0, Exit::Try, e as *const Expr as usize);
            }
            Expr::Consume { place, .. } => self.expr(place),
            Expr::Binary { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            Expr::Call { args, .. }
            | Expr::TryConstruct { args, .. }
            | Expr::Spawn { args, .. } => {
                for a in args {
                    self.expr(a);
                }
            }
            // The scrutinee's own frame, and the handover: `movecheck` marks the
            // row when an arm hands the payload out, so a match that gives its
            // value away has NO step here and the binding the payload flowed
            // into is the one owner there is. The handover is the absence.
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.expr(scrutinee);
                let key = e as *const Expr as usize;
                self.owned_temp(key, "@match", e.line() as u32, |p| {
                    for arm in arms {
                        p.expr(&arm.body);
                    }
                });
            }
            Expr::IfExpr {
                cond,
                then_branch,
                else_branch,
                ..
            } => {
                self.expr(cond);
                self.expr(then_branch);
                if let Some(b) = else_branch {
                    self.expr(b);
                }
            }
            Expr::StructLit { fields, .. } => {
                for (_, v) in fields {
                    self.expr(v);
                }
            }
            Expr::ArrayLit { elems, .. } => {
                for el in elems {
                    self.expr(el);
                }
            }
            Expr::MapLit { entries, .. } => {
                for (k, v) in entries {
                    self.expr(k);
                    self.expr(v);
                }
            }
            Expr::Lambda { body, .. } => match body {
                LambdaBody::Expr(b) => self.expr(b),
                LambdaBody::Block(b) => {
                    // A lambda is its own function in both backends — it lowers
                    // under a shell that owns no release rows — so a `return` or
                    // a `?` inside one unwinds ITS frames and not the enclosing
                    // body's. The frames are set aside rather than shared.
                    let frames = std::mem::take(&mut self.frames);
                    let loops = std::mem::take(&mut self.loops);
                    self.block(b);
                    self.frames = frames;
                    self.loops = loops;
                }
            },
        }
    }
}

fn stmt_line(s: &Stmt) -> usize {
    match s {
        Stmt::Let { line, .. }
        | Stmt::Assign { line, .. }
        | Stmt::SetField { line, .. }
        | Stmt::IndexSet { line, .. }
        | Stmt::Return { line, .. }
        | Stmt::Break { line }
        | Stmt::Continue { line }
        | Stmt::If { line, .. }
        | Stmt::IfLet { line, .. }
        | Stmt::While { line, .. }
        | Stmt::ForIn { line, .. }
        | Stmt::Drop { line, .. }
        | Stmt::Region { line, .. } => *line,
        Stmt::Expr(e) => e.line(),
    }
}

struct FnResult {
    droppable: HashMap<usize, DropKind>,
    holes: HashMap<usize, Vec<String>>,
    notes: Vec<BindingNote>,
}

/// One body's drop sites, in source order.
fn emit_body(body: &Block, lets: &HashMap<usize, LetOwnership>, proto: &Owned) -> FnResult {
    let mut e = Emit {
        droppable: HashMap::new(),
        holes: HashMap::new(),
        notes: Vec::new(),
        region_depth: 0,
        lets,
        proto,
    };
    e.block(body);
    FnResult {
        droppable: e.droppable,
        holes: e.holes,
        notes: e.notes,
    }
}

/// The identity key for a statement: its node address.
fn id(s: &Stmt) -> usize {
    s as *const Stmt as usize
}

/// The walk that finds the `let`s and writes down what happens to each.
///
/// It decides two things of its own — a `region` owns what is allocated inside
/// it, and a string literal is data-segment storage — and reads the rest from
/// [`crate::movecheck`]. There is no expression analysis here at all.
struct Emit<'a> {
    droppable: HashMap<usize, DropKind>,
    /// The places a take took out of a droppable `let` (RFC-0093 M2).
    holes: HashMap<usize, Vec<String>>,
    /// One row per `let`, in source order, with what happens to its value.
    notes: Vec<BindingNote>,
    region_depth: usize,
    /// What every `let` in the program still owns where its block ends.
    lets: &'a HashMap<usize, LetOwnership>,
    /// The `Owned` table — the only thing that decides how a type is released.
    proto: &'a Owned,
}

impl Emit<'_> {
    fn block(&mut self, b: &Block) {
        for s in &b.stmts {
            self.stmt(s);
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Let {
                name, value, line, ..
            } => {
                self.exprs(value);
                let fate = self.fate(id(s), matches!(s, Stmt::Let { mutable: true, .. }), value);
                if let Fate::Reclaimed(kind, holes) = &fate {
                    self.droppable.insert(id(s), kind.clone());
                    if !holes.is_empty() {
                        self.holes.insert(id(s), holes.clone());
                    }
                }
                self.notes.push(BindingNote {
                    name: name.clone(),
                    line: *line,
                    fate,
                });
            }
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                self.exprs(cond);
                self.block(then_block);
                if let Some(eb) = else_block {
                    self.block(eb);
                }
            }
            // Census §14, Phase 10a. `if let Some(s) = f()` matches a value with
            // no name, so nothing released it. It has a row now — `movecheck`
            // writes one keyed by this statement whenever the scrutinee is a
            // TEMPORARY — and the row answers the same question a `let`'s does:
            // did anything take it. A row with no `gone` is a value the arms did
            // not hand on, and releasing it is what closes the row.
            //
            // The release is the whole scrutinee, not the payload: `Option` and
            // `Result` release deeply since Phase 5, and a `None` releases
            // nothing because the walk reads the tag.
            Stmt::IfLet {
                scrutinee,
                then_block,
                else_block,
                line,
                ..
            } => {
                self.exprs(scrutinee);
                // A hole is a path into a RECORD, and a scrutinee is a sum — so
                // `skippable` answers false at the first hop and `fate` says
                // `Leaked` before this can see one. The guard says so, which is
                // what `ForIn` beside it already said; the branch this replaces
                // wrote the hole set into a map neither backend reads, and a
                // corpus-wide probe plus a `consume d.title` written INSIDE an
                // arm confirmed it could not fire.
                if let Fate::Reclaimed(kind, holes) = self.fate(id(s), false, scrutinee) {
                    if holes.is_empty() {
                        self.droppable.insert(id(s), kind);
                    }
                }
                // No `BindingNote`: `vyrn why --memory` lists bindings, and this
                // is a temporary with no name to print.
                let _ = line;
                self.block(then_block);
                if let Some(eb) = else_block {
                    self.block(eb);
                }
            }
            Stmt::While { cond, body, .. } => {
                self.exprs(cond);
                self.block(body)
            }
            // RFC-0092 M5, census "U4's price". `for k in m.keys()` walks a
            // temporary, and `movecheck` gives that temporary the row Phase 10a
            // gave an `if let`'s. The row answers the same question: did an
            // ELEMENT leave the loop. A row with no `gone` is a snapshot the
            // body kept nothing out of, and releasing it — buffer and elements —
            // is what closes the row.
            //
            // A STREAM is refused. `for x in pull()` already closes its producer
            // on every exit path in all three engines (RFC-0075 M2b), so a row
            // here would close it twice.
            Stmt::ForIn { iter, body, .. } => {
                let streaming = self
                    .lets
                    .get(&id(s))
                    .and_then(|r| r.ty.as_ref())
                    .map(|t| crate::types::resolve(t, &self.proto.types))
                    .is_some_and(|t| matches!(t, Type::Stream(_)));
                self.exprs(iter);
                if !streaming {
                    // A hole is a path relative to a RECORD, and nothing a `for`
                    // walks is one — `skippable` therefore answers false and
                    // `fate` says `Leaked` before this ever sees a hole. The
                    // guard says so rather than depending on it.
                    if let Fate::Reclaimed(kind, holes) = self.fate(id(s), false, iter) {
                        if holes.is_empty() {
                            self.droppable.insert(id(s), kind);
                        }
                    }
                }
                // No `BindingNote`: the snapshot is a temporary with no name to
                // print, exactly as an `if let`'s scrutinee is.
                self.block(body)
            }
            Stmt::Region { body, .. } => {
                self.region_depth += 1;
                self.block(body);
                self.region_depth -= 1;
            }
            // A lambda's block body carries `let`s of its own, and the engines
            // walk it as part of this function's AST.
            Stmt::Expr(e) => self.exprs(e),
            Stmt::Assign { value, .. } | Stmt::SetField { value, .. } => self.exprs(value),
            Stmt::IndexSet { index, value, .. } => {
                self.exprs(index);
                self.exprs(value);
            }
            Stmt::Return { value, .. } => {
                if let Some(e) = value {
                    self.exprs(e);
                }
            }
            Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Drop { .. } => {}
        }
    }

    /// Walk the expressions of a statement: any lambda block body inside `e`, so
    /// its `let`s get rows too, and any `match`, so its SCRUTINEE gets one.
    ///
    /// The scrutinee row is the `if let` row (Phase 10a) at the third construct
    /// that walks a temporary. `movecheck` mints it keyed by the match
    /// expression's own address — a match is an expression and has no statement
    /// to key on — and writes on it whenever an arm hands the payload out. A row
    /// that survives is a scrutinee nothing kept, and releasing it is what stops
    /// `match makeResult(i) { .. }` from leaking one every turn.
    fn exprs(&mut self, e: &Expr) {
        // Inside a `region` the arena owns what the region allocated, and the
        // exit hands it back. A scrutinee built there — `match Some(a + b)` —
        // would then have two owners, so this stands aside exactly as the
        // `FreeStr` rule below does. That leaks a scrutinee a CALLEE allocated,
        // which is what a region-enclosed match does today.
        if let (Expr::Match { scrutinee, .. }, 0) = (e, self.region_depth) {
            let key = e as *const Expr as usize;
            // A hole is a path into a RECORD and a scrutinee is not one, so this
            // says what `ForIn` says: a kind with holes is not this row's.
            if let Fate::Reclaimed(kind, holes) = self.fate(key, false, scrutinee) {
                if holes.is_empty() {
                    self.droppable.insert(key, kind);
                }
            }
        }
        match e {
            Expr::Lambda {
                body: LambdaBody::Block(b),
                ..
            } => self.block(b),
            Expr::Lambda {
                body: LambdaBody::Expr(inner),
                ..
            }
            | Expr::Unary { expr: inner, .. }
            | Expr::Try { expr: inner, .. }
            | Expr::Consume { place: inner, .. }
            | Expr::Field { expr: inner, .. } => self.exprs(inner),
            Expr::Binary { lhs, rhs, .. } => {
                self.exprs(lhs);
                self.exprs(rhs);
            }
            Expr::Call { args, .. }
            | Expr::TryConstruct { args, .. }
            | Expr::ArrayLit { elems: args, .. }
            | Expr::Spawn { args, .. } => {
                for a in args {
                    self.exprs(a);
                }
            }
            Expr::StructLit { fields, .. } => {
                for (_, v) in fields {
                    self.exprs(v);
                }
            }
            Expr::MapLit { entries, .. } => {
                for (k, v) in entries {
                    self.exprs(k);
                    self.exprs(v);
                }
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.exprs(scrutinee);
                for a in arms {
                    self.exprs(&a.body);
                }
            }
            Expr::IfExpr {
                cond,
                then_branch,
                else_branch,
                ..
            } => {
                self.exprs(cond);
                self.exprs(then_branch);
                if let Some(eb) = else_branch {
                    self.exprs(eb);
                }
            }
            Expr::Int(_)
            | Expr::Byte(_)
            | Expr::Float(_)
            | Expr::Bool(_)
            | Expr::Str(_)
            | Expr::Var { .. } => {}
        }
    }

    /// Whether the release walk can be told to skip every one of `paths` in a
    /// value of `ty` (RFC-0093 M2).
    ///
    /// The walk carries a path and skips a place whose path is in the set, so
    /// the set has to name places the walk actually visits: a chain of RECORD
    /// fields, each hop resolved through the declarations. Two things end the
    /// chain and both answer false.
    ///
    /// **A declared `release`.** `impl Owned for T` is a user function, and a
    /// function cannot be told to leave one field alone.
    ///
    /// **Anything that is not a record.** An enum's live variant is a runtime
    /// tag, so a hole under a payload is not a place a static walk can skip; an
    /// array's element is chosen by an index the walk does not have. Both leak,
    /// and neither is reachable today — a take of a payload or of an element is
    /// refused (RFC-0093 M1) — so this is the guard for the rule rather than for
    /// the corpus.
    fn skippable(&self, ty: &Type, paths: &[String]) -> bool {
        paths.iter().all(|p| {
            let mut cur = ty.clone();
            for seg in p.split('.') {
                if matches!(self.proto.release_kind(&cur), Some(DropKind::Release(..))) {
                    return false;
                }
                let Type::Record(fields) = crate::types::resolve(&cur, &self.proto.types) else {
                    return false;
                };
                let Some(f) = fields.iter().find(|f| f.name == seg) else {
                    return false;
                };
                cur = f.ty.clone();
            }
            true
        })
    }

    /// What happens to one `let` binding's value — RFC-0089 rule 4, in full.
    ///
    /// The order of the questions is the order a reader needs them. What does
    /// the TYPE release? Nothing, and there is nothing more to say. Something,
    /// and then: does anything else own this storage?
    ///
    /// `key` is the node address the row is keyed by — a `Stmt`'s for a `let`
    /// and for the two statements that walk a temporary, an `Expr`'s for a
    /// `match`. `mutable` is the one thing the STATEMENT still answers: a `let
    /// mut` may end the block holding something other than its initializer.
    fn fate(&self, key: usize, mutable: bool, value: &Expr) -> Fate {
        let row = self.lets.get(&key);
        let bty = row.and_then(|r| r.ty.as_ref());
        let Some(kind) = bty.and_then(|t| self.proto.release_kind(t)) else {
            // A must-use type reaches here BECAUSE it is discharged elsewhere,
            // so "nothing reclaims it" is the wrong sentence about it — see
            // [`Fate::Discharged`]. A `drop` and a move still answer for
            // themselves: each names a line, and a line the reader can go to is
            // worth more than the categorical sentence.
            let Some(linear) = bty.and_then(|t| self.proto.linear_kind(t)) else {
                return Fate::Leaked(Leak::NoRelease {
                    ty: match bty {
                        Some(t) => t.to_string(),
                        None => "unknown".into(),
                    },
                    owns_heap: bty.is_some_and(|t| self.proto.owns_heap(t)),
                });
            };
            return match row.and_then(|r| r.gone.as_ref()) {
                Some(Gone::Dropped { line }) => Fate::Dropped { line: *line },
                Some(Gone::Returned { line }) => Fate::Moved {
                    line: *line,
                    into: "the return".into(),
                },
                Some(Gone::Moved { line, by }) => Fate::Moved {
                    line: *line,
                    into: by.clone(),
                },
                _ => Fate::Discharged(linear),
            };
        };
        // A string literal lives in the data segment. Nothing allocated it and
        // nothing reclaims it (census §1).
        //
        // **The INITIALIZER answers only for a binding whose value cannot
        // change.** `let mut acc: String = ""` is the opening line of every
        // accumulator in this language, and the value the block exits with is
        // whatever the last `acc = acc + …` left — a heap buffer, which this
        // rule used to read as the literal it started as. Measured on the direct
        // backend before the `mut` clause below, a local accumulator grown once
        // a call: 851,968 bytes after 500 calls and 3,211,264 after 2,000
        // (RFC-0096 M3, defect 3).
        //
        // A release of a slot that still holds the literal is not a second
        // defect: `@__vyrn_str_free` reads a `cap` of 0 as "never `realloc`,
        // never free" and returns, and both compiling backends emit a literal
        // that way. So the loop that never runs, and the branch that assigns
        // another literal, both free nothing.
        //
        // This waited on the `region` defect beside it, because releasing a
        // reassigned accumulator is what made a `String` returned out of a
        // `region` reachable — the caller freed a pointer 8 bytes into an arena
        // block and the native heap corrupted. The arena hands out a
        // `__vyrn_malloc` block now (`REGION_RUNTIME`), and a `String` inside a
        // region still answers `Leak::Region` one rule down.
        if matches!(value, Expr::Str(_)) && !mutable {
            return Fate::Static;
        }
        // A dynamic string inside a region is the arena's, and the two
        // mechanisms partition every allocation — nothing is freed twice.
        if kind == DropKind::FreeStr && self.region_depth > 0 {
            return Fate::Leaked(Leak::Region);
        }
        // A `mut` binding is released by its slot's FINAL value in all three
        // engines (Phase 8b), so a declared `release` — ordinary Vyrn that may
        // print — runs on the same value everywhere and a `mut` container
        // reclaims. Nothing refuses a binding for being `mut` any more.
        match row.and_then(|r| r.gone.as_ref()) {
            None => Fate::Reclaimed(kind, Vec::new()),
            Some(Gone::Borrowed(what)) => Fate::Leaked(Leak::Borrowed(what)),
            Some(Gone::Aliased { line }) => Fate::Leaked(Leak::Aliased { line: *line }),
            Some(Gone::Lent { line, to }) => Fate::Leaked(Leak::Escaped {
                callee: to.clone(),
                line: *line,
            }),
            Some(Gone::Captured { line }) => Fate::Leaked(Leak::Captured { line: *line }),
            // RFC-0093 M2. The walk is the type and the type does not know that
            // a place left, so the hole set travels with the verdict and the
            // walk skips exactly these places. Where it cannot be told — a
            // declared `release`, a path that is not a chain of record fields, a
            // hole a later write filled — the whole binding leaks, which is what
            // M1 shipped and the direction this analysis fails in.
            Some(Gone::Hole {
                line,
                paths,
                skippable,
            }) => {
                if *skippable
                    && matches!(kind, DropKind::Deep(_))
                    && bty.is_some_and(|t| self.skippable(t, paths))
                {
                    Fate::Reclaimed(kind, paths.clone())
                } else {
                    Fate::Leaked(Leak::Hole {
                        paths: paths.clone(),
                        line: *line,
                    })
                }
            }
            Some(Gone::Dropped { line }) => Fate::Dropped { line: *line },
            Some(Gone::Returned { line }) => Fate::Moved {
                line: *line,
                into: "the return".into(),
            },
            Some(Gone::Moved { line, by }) => Fate::Moved {
                line: *line,
                into: by.clone(),
            },
        }
    }
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

    /// How many `let`s in function `which` are droppable.
    fn drop_count(src: &str, which: &str) -> usize {
        let (o, _) = analyze_src(src);
        o.droppable.get(which).map(|s| s.len()).unwrap_or(0)
    }

    #[test]
    fn frees_non_escaping_temporary() {
        let src = "fn main() -> Int64 { let a = \"x\"; let b = \"y\"; \
                   let s = a + b; let n = s.length; return n; }";
        assert_eq!(drop_count(src, "main"), 1);
    }

    /// RFC-0089 rule 1, Phase 4c. `let t = s` MOVES: the new name owns the
    /// buffer and the old one no longer does, so the block still frees it once.
    /// Before the rules were enforced this pass could not tell an alias from a
    /// move and left both to leak.
    #[test]
    fn an_alias_moves_the_owner_rather_than_duplicating_it() {
        let src = "fn main() -> Int64 { let a = \"x\"; let b = \"y\"; \
                   let s = a + b; let t = s; return t.length; }";
        assert_eq!(drop_kinds(src, "main"), vec![DropKind::FreeStr]);
    }

    #[test]
    fn concat_argument_is_a_safe_read() {
        let src = "fn main() -> Int64 { let a = \"x\"; let b = \"y\"; \
                   let s = a + b; let u = s + b; return u.length; }";
        assert_eq!(drop_count(src, "main"), 2);
    }

    #[test]
    fn a_value_stored_into_an_outer_container_escapes() {
        // `xs.push(s)` moves `s` into a container that outlives the inner block,
        // so `s` must NOT stay droppable — freeing it would leave the array
        // holding a dangling buffer, and the array releases the element itself
        // since RFC-0092 M2. Only the array is released here.
        let src = "fn main() -> Int64 { let a = \"x\"; let b = \"y\"; \
                   let mut xs: Array<String> = []; \
                   if true { let s = a + b; xs.push(s); } \
                   return xs.length; }";
        assert_eq!(
            drop_kinds(src, "main"),
            vec![DropKind::Deep(Type::Array(Box::new(Type::Str)))]
        );
    }

    #[test]
    fn skips_temporary_inside_region() {
        let src = "fn main() -> Int64 { let a = \"x\"; let b = \"y\"; let mut n = 0; \
                   region { let s = a + b; n = s.length; } return n; }";
        assert_eq!(drop_count(src, "main"), 0);
    }

    /// Census §2c, closed in Phase 4c. `mut` used to mean "who owns the old value
    /// after a reassignment is unclear", so a `mut` String was left to leak. Rule
    /// 1 governs reassignment now, so the binding owns whatever it holds last and
    /// the block frees that.
    #[test]
    fn a_mutable_string_is_reclaimed() {
        let src = "fn main() -> Int64 { let a = \"x\"; let b = \"y\"; \
                   let mut s = a + b; return s.length; }";
        assert_eq!(drop_kinds(src, "main"), vec![DropKind::FreeStr]);
    }

    // ---- ownership transfer ---------------------------------------------

    // ---- shadowing (an inner binder is not the outer binding) ------------

    #[test]
    fn an_inner_let_shadowing_a_string_is_not_a_string() {
        // `s + 1` under `let s = 1` is an integer add. Reading the outer `s`
        // here made `t` droppable, and the backend freed the integer 2.
        let src = "fn main() -> Int64 { let s = \"x\"; print(s); \
                   if true { let s = 1; let t = s + 1; print(t); } return 0; }";
        assert_eq!(drop_count(src, "main"), 0);
    }

    #[test]
    fn a_loop_binder_shadowing_a_string_is_not_a_string() {
        let src = "fn main() -> Int64 { let s = \"x\"; print(s); \
                   let ns: Array<Int64> = []; \
                   for s in ns { let t = s + 1; print(t); } return 0; }";
        // The array, and only the array: `s` is a literal, and `t` is the
        // integer add this test is about. A String `t` here freed an integer.
        assert_eq!(drop_kinds(src, "main"), vec![DropKind::FreeArr]);
    }

    #[test]
    fn a_pattern_binder_shadowing_a_string_is_not_a_string() {
        let src = "fn main() -> Int64 { let s = \"x\"; print(s); \
                   let o: Option<Int64> = Some(1); \
                   if let Some(s) = o { let t = s + 1; print(t); } return 0; }";
        assert_eq!(drop_count(src, "main"), 0);
    }

    #[test]
    fn a_lambda_parameter_shadowing_a_string_is_not_a_string() {
        let src = "fn apply(f: fn(Int64) -> Int64, x: Int64) -> Int64 { return f(x); } \
                   fn main() -> Int64 { let s = \"x\"; print(s); \
                   return apply(s -> { let t = s + 1; print(t); return t + 1; }, 2); }";
        assert_eq!(drop_count(src, "main"), 0);
    }

    #[test]
    fn a_shadowed_string_is_still_freed() {
        // The mirror of the four above. Over-correcting into "an inner binding
        // is never a string" would turn the miscompile into a leak: both
        // concatenations are fresh Strings and both must be reclaimed.
        let src = "fn main() -> Int64 { let a = \"x\"; \
                   let s = a + \"y\"; print(s); \
                   if true { let s = a + \"z\"; print(s); } return 0; }";
        assert_eq!(drop_kinds(src, "main"), vec![DropKind::FreeStr; 2]);
    }

    #[test]
    fn an_inner_let_shadowing_a_non_string_is_a_string() {
        // The other direction of the same lookup: the innermost binding answers,
        // so an inner String under an outer integer concatenates.
        let src = "fn main() -> Int64 { let s = 1; print(s); \
                   if true { let s = \"a\"; let t = s + \"b\"; print(t); } return 0; }";
        assert_eq!(drop_kinds(src, "main"), vec![DropKind::FreeStr]);
    }

    #[test]
    fn factory_returning_concat_is_owned() {
        let src = "fn make(a: String, b: String) -> String { return a + b; } \
                   fn main() -> Int64 { return 0; }";
        let (o, _) = analyze_src(src);
        assert!(o.owned_fns.contains_key("make"));
    }

    #[test]
    fn factory_returning_local_owner_is_owned_and_moves_it() {
        let src = "fn make(a: String, b: String) -> String { let s = a + b; return s; } \
                   fn main() -> Int64 { return 0; }";
        let (o, _) = analyze_src(src);
        assert!(o.owned_fns.contains_key("make"));
        // `s` is moved out by the return, so it is not dropped inside `make`.
        assert_eq!(o.droppable.get("make").map(|s| s.len()).unwrap_or(0), 0);
    }

    /// RFC-0089 M1b. `copy` is a producer, so the copy is the caller's to
    /// release — and the receiver stays a live owner, so both are freed once.
    #[test]
    fn copy_transfers_and_leaves_the_receiver_owned() {
        let src = "fn main() -> Int64 { let a = \"x\" + \"y\"; let b = a.copy(); \
                   print(a); print(b); return 0; }";
        assert_eq!(drop_count(src, "main"), 2);
    }

    /// RFC-0089 rule 3, Phase 4c. A return is owned, so the return TYPE is the
    /// whole answer and the fixpoint that used to look for a borrowed return path
    /// asked a question the language now answers. `movecheck` refuses the
    /// programs this used to describe (`return s` on a `read` parameter).
    #[test]
    fn a_heap_return_type_always_transfers() {
        let src = "fn id(s: String) -> String { return s.copy(); } \
                   fn count(s: String) -> Int64 { return s.byteLength; } \
                   fn main() -> Int64 { return 0; }";
        let (o, _) = analyze_src(src);
        assert_eq!(o.owned_fns.get("id"), Some(&DropKind::FreeStr));
        assert!(!o.owned_fns.contains_key("count"));
    }

    #[test]
    fn caller_frees_owned_call_result() {
        // `y` receives a fresh owned value from `make` and doesn't escape.
        let src = "fn make(a: String, b: String) -> String { return a + b; } \
                   fn main() -> Int64 { let a = \"x\"; let b = \"y\"; \
                       let y = make(a, b); return y.length; }";
        let (o, _) = analyze_src(src);
        assert_eq!(o.droppable.get("main").map(|s| s.len()).unwrap_or(0), 1);
    }

    /// A `read` parameter is a promise that the caller keeps the value (rule 2),
    /// so passing a local to one takes nothing: the caller still frees its own
    /// String, and the callee's result is a second one it owns. Two frees, two
    /// buffers. Before Phase 4c every call was treated as a possible retention
    /// and both leaked.
    #[test]
    fn passing_to_a_read_parameter_keeps_the_caller_the_owner() {
        let src = "fn tail(s: String) -> String { return s + \"!\"; } \
                   fn main() -> Int64 { let a = \"x\"; let b = \"y\"; \
                       let s = a + b; let y = tail(s); return y.length; }";
        assert_eq!(drop_kinds(src, "main"), vec![DropKind::FreeStr; 2]);
    }

    // ---- inferred release for references --------------------------------

    fn drop_kinds(src: &str, which: &str) -> Vec<DropKind> {
        let (o, _) = analyze_src(src);
        o.droppable
            .get(which)
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default()
    }

    // ---- census §14 at a `match`: the scrutinee and its payload ----------

    /// Census §14 at the third construct that walks a temporary. No arm keeps
    /// the scrutinee — both hand back a number — so the match is its last owner
    /// and releases it. `Stmt::IfLet` has had this row since Phase 10a and
    /// `Expr::Match` had none, because a match is an expression and there was no
    /// statement to key one on.
    #[test]
    fn a_match_over_a_temporary_releases_its_scrutinee() {
        let src = "fn maybe(n: Int64) -> Option<String> { return Some(\"x\") } \
                   fn main() -> Int64 { let d = match maybe(1) { \
                   Some(s) => s.byteLength, None => 0, } return Int64(d) }";
        assert_eq!(
            drop_kinds(src, "main"),
            vec![DropKind::Deep(Type::Option(Box::new(Type::Str)))]
        );
    }

    /// The other half of one rule. An arm that hands its payload out gives the
    /// SCRUTINEE up, so the binding the payload flowed into is the only owner
    /// there is — releasing both is the double free that aborted every native
    /// build of `let s = match o { Some(v) => v, None => "" }`.
    #[test]
    fn a_payload_that_leaves_its_arm_leaves_the_scrutinee_unreclaimed() {
        let src = "fn maybe(n: Int64) -> Option<String> { return Some(\"x\") } \
                   fn main() -> Int64 { let o = maybe(1) \
                   let s = match o { Some(v) => v, None => \"\", } \
                   return Int64(s.byteLength) }";
        assert_eq!(drop_kinds(src, "main"), vec![DropKind::FreeStr]);
    }

    /// Inside a `region` the arena owns what the region allocated and the exit
    /// hands it back, so the scrutinee row is not written at all — one block,
    /// one owner.
    #[test]
    fn a_match_inside_a_region_leaves_the_scrutinee_to_the_arena() {
        let src = "fn maybe(n: Int64) -> Option<String> { return Some(\"x\") } \
                   fn main() -> Int64 { let mut t = 0 \
                   region { let d = match maybe(1) { \
                   Some(s) => s.byteLength, None => 0, } t = Int64(d) } return t }";
        assert_eq!(drop_kinds(src, "main"), Vec::new());
    }

    // ---- RFC-0093 M2: a take leaves a hole, and the walk skips it --------

    /// The fate of every binding in `which`, in source order.
    fn fates(src: &str, which: &str) -> Vec<Fate> {
        let (o, _) = analyze_src(src);
        o.notes
            .get(which)
            .map(|ns| ns.iter().map(|n| n.fate.clone()).collect())
            .unwrap_or_default()
    }

    /// The hole set of the binding named `name`, or `None` where it leaks.
    fn holes_of(src: &str, name: &str) -> Option<Vec<String>> {
        let (o, _) = analyze_src(src);
        let n = o
            .notes
            .get("main")
            .unwrap()
            .iter()
            .find(|n| n.name == name)
            .unwrap();
        match &n.fate {
            Fate::Reclaimed(_, holes) => Some(holes.clone()),
            _ => None,
        }
    }

    const DOC: &str = "type Doc = { title: String, body: String } \
                       fn mk(a: String) -> Doc { return Doc { title: a + \"t\", body: a + \"b\" } } ";

    #[test]
    fn a_taken_field_is_the_only_place_the_walk_skips() {
        let src = format!(
            "{DOC} fn main() -> Int64 {{ let d = mk(\"x\"); let t = consume d.title; \
             return Int64(t.byteLength) + Int64(d.body.byteLength); }}"
        );
        assert_eq!(holes_of(&src, "d"), Some(vec!["title".to_string()]));
    }

    /// The hole is a SET. `std/vyx.vyrn:1431` drains nine fields out of one
    /// record, and `took` writes only where nothing is written yet — so a second
    /// take had to stop going through it.
    #[test]
    fn every_take_of_one_record_joins_the_hole_set() {
        let src = format!(
            "{DOC} fn main() -> Int64 {{ let d = mk(\"x\"); let t = consume d.title; \
             let b = consume d.body; return Int64(t.byteLength) + Int64(b.byteLength); }}"
        );
        assert_eq!(
            holes_of(&src, "d"),
            Some(vec!["title".to_string(), "body".to_string()])
        );
    }

    /// The path is relative to the binding and may be more than one hop:
    /// `std/vyx.vyrn:4091` writes `consume hs.head.err`.
    #[test]
    fn a_hole_can_be_a_chain_of_fields() {
        let src = "type Inner = { err: String, n: Int64 } \
                   type Outer = { head: Inner, tail: String } \
                   fn mk(a: String) -> Outer { return Outer { head: Inner { err: a + \"e\", n: 1 }, tail: a + \"l\" } } \
                   fn main() -> Int64 { let hs = mk(\"y\"); let e = consume hs.head.err; \
                   return Int64(e.byteLength) + Int64(hs.tail.byteLength); }";
        assert_eq!(holes_of(src, "hs"), Some(vec!["head.err".to_string()]));
    }

    /// A write fills the hole, and the store that fills it releases what the
    /// place held — the buffer the take gave away. So the binding leaks whole
    /// rather than skipping a place that is no longer empty.
    #[test]
    fn a_write_after_a_take_leaks_the_whole_binding() {
        let src = format!(
            "{DOC} fn main() -> Int64 {{ let mut d = mk(\"x\"); let t = consume d.title; \
             d.title = \"z\"; return Int64(t.byteLength) + Int64(d.title.byteLength); }}"
        );
        assert!(
            matches!(holes_of(&src, "d"), None),
            "a filled hole must not be skipped: {:?}",
            fates(&src, "main")
        );
    }

    /// A hole is not the last word. Every later row says the value LEFT, and
    /// those win — `gave_up` marks the root of every name a `return` expression
    /// reads, so a return after a take records that the whole binding went.
    ///
    /// The wasm generator engine trapped on `std/vyx` while this rule was the
    /// other way round, and three-way parity passed through the same bug: the
    /// binding was released here AND held by the caller.
    #[test]
    fn a_return_after_a_take_beats_the_hole() {
        let src = format!(
            "{DOC} fn take(a: String) -> String {{ let d = mk(a); return consume d.title; }} \
             fn main() -> Int64 {{ return take(\"x\").byteLength; }}"
        );
        let (o, _) = analyze_src(&src);
        let n = o.notes.get("take").unwrap().iter().find(|n| n.name == "d");
        assert!(
            matches!(n.map(|n| &n.fate), Some(Fate::Moved { .. })),
            "{:?}",
            n.map(|n| &n.fate)
        );
        assert!(o.droppable.get("take").unwrap().is_empty());
    }

    /// A declared `release` is a user function, and a function cannot be told to
    /// leave one field alone.
    #[test]
    fn a_declared_release_keeps_leaking_its_hole() {
        let src = "protocol Owned { fn release(self) } \
                   type Box = { name: String, n: Int64 } \
                   impl Owned for Box { fn release(self) { print(1) } } \
                   fn mk(a: String) -> Box { return Box { name: a + \"n\", n: 1 } } \
                   fn main() -> Int64 { let b = mk(\"x\"); let t = consume b.name; \
                   return Int64(t.byteLength) + b.n; }";
        assert!(
            matches!(holes_of(src, "b"), None),
            "{:?}",
            fates(src, "main")
        );
    }

    // ---- auto-free for mutable arrays -----------------------------------

    #[test]
    fn mut_array_with_self_update_is_auto_freed() {
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = []; \
                   let mut i = 0; while i < 3 { a.push(i); i = i + 1; } \
                   return a[0]; }";
        assert_eq!(drop_kinds(src, "main"), vec![DropKind::FreeArr]);
    }

    #[test]
    fn explicitly_dropped_array_is_not_auto_freed() {
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = []; \
                   a.push(1); let v = a[0]; drop a; return v; }";
        assert_eq!(drop_count(src, "main"), 0);
    }

    #[test]
    fn returned_array_is_not_auto_freed() {
        let src = "fn build() -> Array<Int64> { let mut a: Array<Int64> = []; \
                   a.push(1); return a; } fn main() -> Int64 { return 0; }";
        // `a` is moved out by the return, so it is not freed inside `build`.
        assert_eq!(drop_count(src, "build"), 0);
    }

    // ---- the type answers, not the expression (RFC-0086 M1) --------------

    #[test]
    fn an_annotated_array_literal_is_released() {
        // The defect the RFC was written from: `Expr::ArrayLit` was absent from
        // the expression list, so this leaked on every engine while the identical
        // `array()` call did not. Nothing forced the two to agree, because the
        // list was what decided.
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = []; \
                   a.push(1); return a[0]; }";
        assert_eq!(drop_kinds(src, "main"), vec![DropKind::FreeArr]);
    }

    #[test]
    fn an_unannotated_array_literal_is_not_released() {
        // The other half, and the one that costs a heap if it is got wrong.
        // `[1, 2, 3]` with no annotation is a FIXED array held inline, so the
        // literal cannot say what it is — only the annotation can. Answering
        // `Array` for every literal freed a stack address and corrupted the heap.
        let src = "fn main() -> Int64 { let a = [1, 2, 3]; return a[0]; }";
        assert_eq!(drop_count(src, "main"), 0);
    }

    #[test]
    fn a_self_referring_array_element_is_not_walked() {
        // `type L = Array<L>` has no bottom to a structural walk, and both
        // compiling backends emitted one until they ran out of stack — the same
        // crash `copy` met in Phase 4b. The row answers the buffer alone, so the
        // elements leak, which is the answer this file gives wherever it cannot
        // prove otherwise.
        let src = "type L = Array<L>; fn main() -> Int64 { let xs: L = []; return xs.length; }";
        assert_eq!(drop_kinds(src, "main"), vec![DropKind::FreeArr]);
    }

    #[test]
    fn a_fresh_key_snapshot_is_released() {
        // `m.keys()` copies the keys into a new buffer (RFC-0028, and the KEYS
        // themselves since RFC-0092 M2) and was absent from the same list.
        let src = "fn main() -> Int64 { let m: Map<String, Int64> = [\"a\": 1]; \
                   let ks = m.keys(); return ks.length; }";
        // The map and the snapshot, in whichever order the map iterates. Both
        // are `Deep`: the snapshot's elements are Strings with a release row
        // (U4, M2), and since M3 so are the map's own keys.
        let mut kinds = drop_kinds(src, "main");
        kinds.sort_by_key(|k| format!("{k:?}"));
        assert_eq!(
            kinds,
            vec![
                DropKind::Deep(Type::Array(Box::new(Type::Str))),
                DropKind::Deep(Type::Map(Box::new(Type::Str), Box::new(Type::Int))),
            ]
        );
    }

    /// RFC-0095 M3, and RFC-0092 M5's row one keyword over.
    ///
    /// `for x in consume xs` takes the buffer, so the binding's row says `Moved`
    /// and the block releases nothing — which was the truth and the whole leak.
    /// The LOOP is the last owner and gets the row a snapshot gets.
    #[test]
    fn a_consuming_loop_releases_what_it_took() {
        let src = "fn make() -> Array<String> { let mut o: Array<String> = []; \
                   o.push(\"a\"); return o; } \
                   fn main() -> Int64 { let xs = make(); let mut n = 0; \
                   for x in consume xs { n = n + Int64(x.byteLength); } return n; }";
        // One row, and it is the loop's: the `let` is `Moved`, so it has none.
        assert_eq!(
            drop_kinds(src, "main"),
            vec![DropKind::Deep(Type::Array(Box::new(Type::Str)))]
        );
        // A body that hands an element on marks the row gone, and the whole
        // container leaks — a leak, never a double free.
        let src = "fn make() -> Array<String> { let mut o: Array<String> = []; \
                   o.push(\"a\"); return o; } \
                   fn main() -> Int64 { let xs = make(); let mut out: Array<String> = []; \
                   for x in consume xs { out.push(x); } return out.length; }";
        assert_eq!(
            drop_kinds(src, "main"),
            vec![DropKind::Deep(Type::Array(Box::new(Type::Str)))],
            "only `out` may be released here"
        );
    }

    #[test]
    fn a_bare_file_with_no_imports_still_frees_its_string() {
        // The bootstrap answer. `vyrn run` on a bare file has no resolver and
        // therefore no `std/`, so the built-in rows are seeded by the compiler and
        // this program — which imports nothing and declares no protocol — still
        // gets a `free`. RFC-0080 M3 refused `?` through a std protocol for this
        // exact reason; the decision that frees memory may not be weaker.
        let src = "fn main() -> Int64 { let a = \"x\"; let s = a + \"y\"; \
                   return s.byteLength; }";
        assert!(!src.contains("import"));
        assert_eq!(drop_kinds(src, "main"), vec![DropKind::FreeStr]);
    }

    #[test]
    fn a_user_type_declares_how_it_is_released() {
        // The design's own test, in miniature: nothing in the compiler knows the
        // name `Ring`. The row comes out of the program.
        let src = "protocol Owned { fn release(self) } \
                   type Ring = { slots: Array<Int64> } \
                   impl Owned for Ring { fn release(self) { print(1) } } \
                   fn make() -> Ring { return Ring { slots: [] } } \
                   fn main() -> Int64 { let r = make(); return 0; }";
        let (o, _) = analyze_src(src);
        assert_eq!(
            o.owned_fns.get("make"),
            Some(&DropKind::Release(
                "Owned__Ring__release".to_string(),
                Type::Named("Ring".into())
            ))
        );
        assert_eq!(
            drop_kinds(src, "main"),
            vec![DropKind::Release(
                "Owned__Ring__release".to_string(),
                Type::Named("Ring".into())
            )]
        );
    }

    /// RFC-0086 M3, the same test one protocol over: nothing in the compiler
    /// knows the name `Txn`, and the obligation comes out of the program.
    #[test]
    fn a_user_type_declares_that_it_must_be_used() {
        let src = "protocol MustUse {} \
                   type Txn = { id: Int64 } \
                   impl MustUse for Txn {} \
                   type Plain = { id: Int64 } \
                   fn main() -> Int64 { return 0 }";
        let (_, p) = analyze_src(src);
        let owned = Owned::new(&p);
        let txn = Type::Named("Txn".into());
        assert_eq!(
            owned.linear_kind(&txn),
            Some(Linear::Declared("Txn".into()))
        );
        assert_eq!(owned.linear_kind(&Type::Named("Plain".into())), None);
        // RFC-0092 M4: a container answers what its element answers, and the row
        // still names the type that DECLARED it rather than the container.
        assert_eq!(
            owned.linear_kind(&Type::Array(Box::new(txn.clone()))),
            Some(Linear::Declared("Txn".into()))
        );
        assert_eq!(
            owned.linear_kind(&Type::Option(Box::new(Type::Map(
                Box::new(Type::Str),
                Box::new(txn)
            )))),
            Some(Linear::Declared("Txn".into()))
        );
        assert_eq!(
            owned.linear_kind(&Type::Array(Box::new(Type::Named("Plain".into())))),
            None
        );
        // A type PARAMETER is not an obligation, which is what keeps every
        // generic container in the corpus working.
        assert_eq!(
            owned.linear_kind(&Type::Array(Box::new(Type::Param("T".into())))),
            None
        );
        // And the seeded row is still there for a program that declares nothing,
        // which is the bootstrap answer `Owned` gives above: a bare file has no
        // resolver, so `Stream` may not depend on one.
        assert_eq!(
            Owned::default().linear_kind(&Type::Stream(Box::new(Type::Int))),
            Some(Linear::Stream)
        );
        // RFC-0095 M1's seeded row, and the two facts a task adds. It is linear
        // whatever it carries, and it OWNS HEAP whatever it carries — the frame,
        // the record and the operating-system handle are there for a
        // `Task<Unit>` exactly as for a `Task<String>`. Neither is reclaimed by
        // the automatic path: `release_kind` answers `None`, because the
        // construct that discharges a task is what releases it.
        let bare = Owned::default();
        for inner in [Type::Int, Type::Str, Type::Unit] {
            let t = Type::Task(Box::new(inner));
            assert_eq!(bare.linear_kind(&t), Some(Linear::Task));
            assert!(bare.owns_heap(&t));
            assert_eq!(bare.release_kind(&t), None);
        }
        // And a container carries it, which is RFC-0092 M4's rule reaching one
        // type further.
        assert_eq!(
            bare.linear_kind(&Type::Array(Box::new(Type::Task(Box::new(Type::Int))))),
            Some(Linear::Task)
        );
    }

    /// RFC-0092 M3: a record releases its places. Phase 5 measured that it could
    /// not, and [`Owned::release_kind`] records the three parity failures a row
    /// produced then — a record hands its insides out as projections, and rule 3
    /// recorded a returned projection as a lend rather than refusing it. M1
    /// refuses all three spellings, so the row is sayable.
    #[test]
    fn a_record_releases_its_places() {
        let src = "type Ring = { slots: Array<Int64> } \
                   fn make() -> Ring { return Ring { slots: [] } } \
                   fn main() -> Int64 { let r = make(); return 0; }";
        let (o, _) = analyze_src(src);
        let want = DropKind::Deep(Type::Record(vec![Field {
            name: "slots".into(),
            ty: Type::Array(Box::new(Type::Int)),
        }]));
        assert_eq!(o.owned_fns.get("make"), Some(&want));
        assert_eq!(drop_kinds(src, "main"), vec![want]);
    }

    /// And a record of scalars is not, which keeps the rule about heap.
    #[test]
    fn a_record_of_scalars_is_reclaimed_by_nothing() {
        let src = "type Point = { x: Int64, y: Int64 } \
                   fn make() -> Point { return Point { x: 1, y: 2 } } \
                   fn main() -> Int64 { let p = make(); return 0; }";
        let (o, _) = analyze_src(src);
        assert!(!o.owned_fns.contains_key("make"));
        assert_eq!(drop_count(src, "main"), 0);
    }

    /// The guard the `Array` row already carried, one shape over. `type Node =
    /// { kids: Array<Node> }` is ordinary Vyrn, and a structural release walk of
    /// it has no bottom — the crash `copy` met in Phase 4b, met a third time.
    /// It answers nothing and its places leak, which is what this file does
    /// wherever it cannot prove otherwise.
    #[test]
    fn a_self_referring_record_is_not_walked() {
        let src = "type Node = { name: String, kids: Array<Node> } \
                   fn main() -> Int64 { let n = Node { name: \"a\", kids: [] }; \
                   return n.kids.length; }";
        assert_eq!(drop_count(src, "main"), 0);
    }

    /// RFC-0096. The same shape with a DECLARATION on the cycle is walked
    /// again: the release emits a call at `Node`, and a call is the bottom the
    /// structural walk lacked. The record above it — which only REACHES the
    /// self-referring type — gets its row back with it, which is 63 corpus
    /// bindings on two `impl`s.
    #[test]
    fn a_declaration_on_the_cycle_gives_the_types_above_it_their_row_back() {
        let decl = "type Node = { name: String, kids: Array<Node> } \
                    type Doc = { root: Node, title: String } \
                    impl Owned for Node { fn release(consume self) { \
                    let name = consume self.name; drop name; \
                    let kids = consume self.kids; drop kids; } } ";
        let src = format!(
            "{decl} fn main() -> Int64 {{ \
             let d = Doc {{ root: Node {{ name: \"a\", kids: [] }}, title: \"t\" }}; \
             return d.root.kids.length; }}"
        );
        let (o, _) = analyze_src(&src);
        assert_eq!(
            o.proto.release_kind(&Type::Named("Node".into())),
            Some(DropKind::Release(
                "Owned__Node__release".to_string(),
                Type::Named("Node".into())
            ))
        );
        // The container and the record above the declaration walk again.
        assert!(matches!(
            o.proto
                .release_kind(&Type::Array(Box::new(Type::Named("Node".into())))),
            Some(DropKind::Deep(_))
        ));
        assert!(matches!(
            o.proto.release_kind(&Type::Named("Doc".into())),
            Some(DropKind::Deep(_))
        ));
        assert_eq!(drop_count(&src, "main"), 1);
    }

    /// RFC-0093's hole. `consume d.title` takes one field and leaves the record
    /// behind. M1 left the whole binding unreclaimed rather than free the field
    /// twice; M2 carries the hole set to the walk, so the record is reclaimed
    /// MINUS the place the take gave away.
    #[test]
    fn a_record_with_a_taken_field_is_reclaimed_minus_the_hole() {
        let src = "type Doc = { title: String, body: String } \
                   fn main() -> Int64 { let d = Doc { title: \"a\" + \"b\", body: \"c\" }; \
                   let t = consume d.title; return t.byteLength; }";
        // `t` is the String the record gave away, and `d` is the rest of it.
        assert_eq!(
            fates(src, "main"),
            vec![
                Fate::Reclaimed(
                    DropKind::Deep(Type::Record(vec![
                        Field {
                            name: "title".into(),
                            ty: Type::Str,
                        },
                        Field {
                            name: "body".into(),
                            ty: Type::Str,
                        },
                    ])),
                    vec!["title".to_string()]
                ),
                Fate::Reclaimed(DropKind::FreeStr, Vec::new()),
            ]
        );
    }

    /// `rfcs/census-regions.md` defect 2. A `Task<T>` has no release row on
    /// purpose, and the report read that as a leak — about a task the next line
    /// joins. The obligation is proved elsewhere, so the sentence is
    /// categorical; a `drop` still answers with its own line.
    #[test]
    fn a_discharged_task_is_not_a_leak() {
        let src = "fn work(n: Int64) -> Int64 { return n + 1 } \
                   fn main() -> Int64 { let t = spawn work(1) let u = spawn work(2) \
                   let n = t.join() drop u return n }";
        let f = fates(src, "main");
        assert_eq!(f[0], Fate::Discharged(Linear::Task), "{f:?}");
        assert!(matches!(f[1], Fate::Dropped { .. }), "{f:?}");
        assert_eq!(
            f[0].words(),
            "discharged, not leaked — a task is joined, forwarded or dropped on every path, \
             and that lowering frees it"
        );
    }

    /// Census §14, Phase 5. An `Option` and a `Result` DO own their payload:
    /// the recommended way to write a fallible function was also the leaking one.
    #[test]
    fn a_sum_owns_its_payload() {
        let src = "fn pick(a: String, b: String) -> Option<String> { return Some(a + b); } \
                   fn main() -> Int64 { let a = \"x\"; let b = \"y\"; \
                       let o = pick(a, b); return 0; }";
        let (o, _) = analyze_src(src);
        let want = DropKind::Deep(Type::Option(Box::new(Type::Str)));
        assert_eq!(o.owned_fns.get("pick"), Some(&want));
        assert_eq!(drop_kinds(src, "main"), vec![want]);
    }

    /// And an `Option` of a scalar is not, which keeps the rule about heap.
    #[test]
    fn a_sum_of_scalars_is_reclaimed_by_nothing() {
        let src = "fn pick(n: Int64) -> Option<Int64> { return Some(n); } \
                   fn main() -> Int64 { let o = pick(1); return 0; }";
        let (o, _) = analyze_src(src);
        assert!(!o.owned_fns.contains_key("pick"));
        assert_eq!(drop_count(src, "main"), 0);
    }

    /// A type that reaches itself owns heap, because it had to be boxed to be
    /// representable at all.
    ///
    /// THE DEFECT THIS PINS: the walk used to give up after eight levels and
    /// answer `false`, so `type Tree = | Leaf | Node(Tree, Tree)` — which is
    /// nothing but heap — reported that it owned none. `release_enum` skips a
    /// variant whose payloads own nothing, so the boxes behind `Node` were
    /// never freed and 200,000 trees of depth 8 peaked at 3.1 GB against a live
    /// set of one. See `rfcs/census/declared-release-does-not-run.md`.
    ///
    /// The depth counter is the thing to watch. Any bound cheap enough to reach
    /// on an ordinary nested type brings the leak back, and it comes back
    /// SILENTLY: nothing fails, the program only grows.
    #[test]
    fn a_self_referring_type_owns_heap() {
        use crate::ast::{EnumVariant, TypeDecl};
        let mut types: HashMap<String, TypeDecl> = HashMap::new();
        types.insert(
            "Tree".to_string(),
            TypeDecl {
                name: "Tree".to_string(),
                base: Type::Enum(vec![
                    EnumVariant {
                        name: "Leaf".to_string(),
                        payload: vec![],
                    },
                    EnumVariant {
                        name: "Node".to_string(),
                        payload: vec![
                            Type::Named("Tree".to_string()),
                            Type::Named("Tree".to_string()),
                        ],
                    },
                ]),
                exported: false,
                module: None,
                doc: None,
                type_params: vec![],
                predicate: None,
                line: 1,
            },
        );
        assert!(
            super::owns_heap(&Type::Named("Tree".to_string()), &types),
            "a recursive enum owns the boxes its payloads travel in"
        );
        // And a record that reaches itself, which is the other shape `own.rs`
        // documents (`type Node = {{ kids: Array<Node> }}`) — that one owns heap
        // through the array as well, so it answered `true` before; this is the
        // enum shape, where the box IS the only heap.
        assert!(
            !super::owns_heap(&Type::Int, &types),
            "an integer owns nothing, and the cycle rule must not change that"
        );
    }

    // ---- the RFC-0089 gate (M0) ------------------------------------------

    /// The RFC-0089 rule-1 predicate, now a public function so the checker and
    /// both backends ask it too (`copy` copies exactly what this counts).
    fn owns_heap(ty: &Type, types: &HashMap<String, TypeDecl>, _depth: usize) -> bool {
        super::owns_heap(ty, types)
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
        let mut reasons: HashMap<&'static str, usize> = HashMap::new();
        let (mut total, mut kept) = (0usize, 0usize);

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
            let own = analyze(&program);
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

            for notes in own.notes.values() {
                for n in notes {
                    total += 1;
                    match &n.fate {
                        Fate::Leaked(r) => *reasons.entry(r.kind()).or_default() += 1,
                        _ => kept += 1,
                    }
                }
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
        println!(
            "bindings: {total} — {kept} reclaimed/moved/dropped/discharged/static, \
             {leaks} not reclaimed"
        );
        for (reason, count) in rows {
            println!("  {count:>5}  {reason}");
        }
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
