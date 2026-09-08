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
#[derive(Clone, Default)]
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
            Type::Array(e) | Type::ArrayN(e, _) | Type::SmallArray(e, _) => self.linear_kind(&e),
            Type::Map(a, b) => self.linear_kind(&a).or_else(|| self.linear_kind(&b)),
            // The two built-in sums, read through their variant lists since
            // RFC-0126 §8.11's M4b. A DECLARED enum is left alone for the reason
            // the record field is: `impl MustUse for Order` is how an author says
            // one holding a `Txn` must be discharged.
            ref r if crate::types::option_payload(r).is_some() => {
                self.linear_kind(crate::types::option_payload(r).unwrap())
            }
            ref r if crate::types::result_payloads(r).is_some() => {
                let (a, b) = crate::types::result_payloads(r).unwrap();
                self.linear_kind(a).or_else(|| self.linear_kind(b))
            }
            _ => None,
        }
    }

    /// Whether a value of `ty` has to be handed on by name — letting it go out
    /// of scope is an error. [`Owned::linear_kind`] with the row forgotten.
    pub fn must_use(&self, ty: &Type) -> bool {
        self.linear_kind(ty).is_some()
    }

    /// The program's nominal declarations, read once when this table was built.
    ///
    /// [`crate::types::decl_map`] CLONES every declaration of the program, and a
    /// pass that calls it per node pays the whole program's type table at every
    /// node. This table already holds that map — every rule above resolves
    /// through it — so a pass that has an `Owned` has the declarations too, and
    /// has them once (RFC-0125 §3 M3, the placer's cost).
    pub fn types(&self) -> &HashMap<String, TypeDecl> {
        &self.types
    }

    /// Whether `ty` transitively owns heap, against this program's declarations.
    ///
    /// Not the same question as [`Owned::release_kind`] answering `Some`, and
    /// the gap between the two is every row RFC-0092 M3 adds: a record owns two
    /// Strings and has no release rule. The report needs both answers to
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
    /// Whether releasing a value of `ty` could CALL a declared `impl Owned`
    /// release — the per-type question a coarse "does ANY type declare one"
    /// gate approximated (the upgrade the record-producer gate's comment
    /// named). A walk that cannot reach a declaration is silent whatever it
    /// frees, so a receiver temporary of such a type may die at its last
    /// read without user-visible timing. Conservative where it cannot see:
    /// a type variable, a stored `fn`, a `lazy` capture block and a task all
    /// answer yes.
    pub fn reaches_declared(&self, ty: &Type) -> bool {
        let mut seen: std::collections::HashSet<String> = Default::default();
        self.reaches_declared_in(ty, &mut seen)
    }

    fn reaches_declared_in(&self, ty: &Type, seen: &mut std::collections::HashSet<String>) -> bool {
        if let Some(k) = crate::types::type_key(ty) {
            if self.impls.contains_key(&k) {
                return true;
            }
            if !seen.insert(k) {
                // A cycle without a declaration on it: this path is done.
                return false;
            }
        }
        match crate::types::resolve(ty, &self.types) {
            Type::Array(t)
            | Type::ArrayN(t, _)
            | Type::SmallArray(t, _)
            | Type::Stream(t)
            | Type::Partial(t) => self.reaches_declared_in(&t, seen),
            Type::Merge(a, b) => {
                self.reaches_declared_in(&a, seen) || self.reaches_declared_in(&b, seen)
            }
            Type::Map(k, v) => {
                self.reaches_declared_in(&k, seen) || self.reaches_declared_in(&v, seen)
            }
            Type::Record(fields) => fields.iter().any(|f| self.reaches_declared_in(&f.ty, seen)),
            Type::Enum(vs) => vs
                .iter()
                .flat_map(|v| v.payload.iter())
                .any(|p| self.reaches_declared_in(p, seen)),
            Type::App(_, args) => args.iter().any(|a| self.reaches_declared_in(a, seen)),
            Type::Param(_) | Type::Fn(..) | Type::Lazy(_) | Type::Task(_) => true,
            _ => false,
        }
    }

    /// Whether `name` IS a declared `release` body. A consume-match inside
    /// one must not free its payload boxes: the CALLER of a declared release
    /// walks them afterward (`release_enum` with `payloads` false), and that
    /// is the one place the walk and the match would meet twice.
    pub fn is_release_fn(&self, name: &str) -> bool {
        self.impls.values().any(|f| f == name)
    }

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
            // A PARAM element answers Deep too (round forty-nine): inside a
            // generic body the element is unknowable, the emitters substitute
            // the instance's type before walking, and a walk over heap-free
            // elements frees nothing — `drop vals` in `impl<T> Owned for
            // Slots<T>` was buffer-only under the shared generic row and the
            // String instance leaked its elements.
            Type::Array(e) => Some(
                if self.unbounded(&e).is_none()
                    && (self.release_kind(&e).is_some()
                        || matches!(crate::types::resolve(&e, &self.types), Type::Param(_)))
                {
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
            // Census §14's two rows are the enum's row now: since RFC-0126
            // §8.11's M4b a built-in sum RESOLVES to `| None | Some(T)` and
            // `| Err(E) | Ok(T)`, so it takes the arm below, with the guard a
            // declared enum already had.
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
            //
            // `Code` (RFC-0054) is the one BUILT-IN name that arrives here,
            // and its `None` is a decision rather than a default — see the
            // note [`owns_heap`] carries at the same line.
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
            Type::Array(t)
            | Type::ArrayN(t, _)
            | Type::SmallArray(t, _)
            | Type::Lazy(t)
            | Type::Task(t)
            | Type::Stream(t) => deeper(t),
            Type::Map(a, b) => deeper(a).or_else(|| deeper(b)),
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
            // A name with no declaration owns nothing, and `Code` (RFC-0054)
            // is the one BUILT-IN such name — the answer is stated here
            // rather than assumed (RFC-0125 §3 M3, the last table's slice).
            // A `Code` is a HANDLE: an index into the piece arena, which is
            // the interpreter's whether the generator runs interpreted or
            // compiled. The compiled route holds the index as an `i64` and
            // reaches the arena through `vyrn_gen` imports, and that import
            // list — `text`, `splice`, `rawAt`, `concat`, `render` — has no
            // release in it, because there is nothing guest-side to release.
            // So `Code` owns no buffer, and the temporary a `Code`-valued
            // argument would need is one no engine could free.
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
            Type::ArrayN(t, _) | Type::Lazy(t) => deeper(&t),
            Type::Record(fs) => fs.iter().any(|f| deeper(&f.ty)),
            // A SUM whose payload travels BOXED owns that box whatever else the
            // payload holds — `Option<Handle>` is three words behind one pointer
            // (round twenty-nine, the same argument the recursive-name rule above
            // already made: the representation's box IS heap).
            //
            // One question, asked once, of every sum — `types::payload_boxed`,
            // which is the emitter's own rule. It was written out here four
            // times, as a hand-kept word list, and RFC-0126 §8.9 moved the rule
            // out from under it: a payload two words wide rides in its slots now
            // and the list still called it boxed (§8.11 recorded the drift).
            // Since §8.11's M4b there is one arm as well as one rule: the two
            // built-in sums resolve to their variant lists.
            Type::Enum(vs) => vs.iter().any(|v| {
                v.payload
                    .iter()
                    .any(|t| crate::types::payload_boxed(t, types) || deeper(t))
            }),
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

/// Whether the release walk of a value of `ty` can be told to skip every one
/// of `paths` (RFC-0093 M2).
///
/// The walk carries a path and skips a place whose path is in the set, so the
/// set has to name places the walk actually visits: a chain of RECORD fields,
/// each hop resolved through the declarations. Two things end the chain and
/// both answer false.
///
/// **A declared `release`.** `impl Owned for T` is a user function, and a
/// function cannot be told to leave one field alone.
///
/// **Anything that is not a record.** An enum's live variant is a runtime tag,
/// so a hole under a payload is not a place a static walk can skip; an array's
/// element is chosen by an index the walk does not have. Both leak, and
/// neither is reachable today — a take of a payload or of an element is
/// refused (RFC-0093 M1) — so this is the guard for the rule rather than for
/// the corpus.
///
/// The CORE asks it too, where it states a binding's hole at the `consume`
/// that made it: a hole the walk cannot skip must not be stated, or a `drop`
/// of the binding would walk around a place its declared `release` frees
/// anyway (`tests/refusals/r22_drop_with_a_hole.vyrn`).
pub fn skippable(proto: &Owned, ty: &Type, paths: &[String]) -> bool {
    paths.iter().all(|p| {
        let mut cur = ty.clone();
        for seg in p.split('.') {
            if matches!(proto.release_kind(&cur), Some(DropKind::Release(..))) {
                return false;
            }
            let Type::Record(fields) = crate::types::resolve(&cur, &proto.types) else {
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
/// call graph, and [`ReleasePlan::arg_drops`] is what the backends ask. A String
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
    /// Functions whose return value transfers heap ownership to the caller,
    /// with the kind of value returned.
    ///
    /// Since RFC-0089 rule 3 this is the return type and nothing else: a return
    /// is owned, and `movecheck` refuses the program where it is not. The
    /// fixpoint that used to compute it asked a question the language now
    /// answers.
    pub owned_fns: HashMap<String, DropKind>,
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
    /// The two closures over the call graph, handed on so the CORE can ask
    /// [`crate::movecheck::arg_verdict`] the same question at the same
    /// position (RFC-0125 §3 M3, the argument slice).
    ///
    /// Not a table: neither says anything a body states. `lending` names the
    /// functions whose result the caller must not release and `retains` the
    /// positions that KEEP a borrowed parameter, and both are answers only a
    /// pass that has read every body can give. See
    /// [`crate::movecheck::Facts::lending`] for why they still exist.
    pub lending: std::collections::HashSet<String>,
    pub retains: std::collections::HashSet<(String, usize)>,
    pub escapers: std::collections::HashSet<String>,
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
    // Rule 3: a return is owned. The return type is the whole answer.
    let owned_fns: HashMap<String, DropKind> = program
        .functions
        .iter()
        .filter_map(|f| proto.release_kind(&f.ret).map(|k| (f.name.clone(), k)))
        .collect();
    let plan = ReleasePlan::default();
    let mut ownership = Ownership {
        plan,
        owned_fns,
        memory: HashMap::new(),
        proto,
        // Every row in this table is the placer's now: the analysis injects
        // none, and the fold that did is deleted (RFC-0125 §3 M3).
        releases: HashMap::new(),
        lending: facts.lending.clone(),
        retains: facts.retains.clone(),
        escapers: facts.escapers.clone(),
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

    #[test]
    fn factory_returning_concat_is_owned() {
        let src = "fn make(a: String, b: String) -> String { return a + b; } \
                   fn main() -> Int64 { return 0; }";
        let (o, _) = analyze_src(src);
        assert!(o.owned_fns.contains_key("make"));
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

    // ---- census §14 at a `match`: the scrutinee and its payload ----------

    /// `Option<T>`'s row as `release_kind` records it since RFC-0126 §8.11's M4b:
    /// the RESOLVED type, which is the variant list `resolve` answers.
    fn opt_row(t: Type) -> DropKind {
        DropKind::Deep(Type::Enum(vec![
            EnumVariant {
                name: "None".to_string(),
                payload: Vec::new(),
            },
            EnumVariant {
                name: "Some".to_string(),
                payload: vec![t],
            },
        ]))
    }

    // ---- auto-free for mutable arrays -----------------------------------

    // ---- the type answers, not the expression (RFC-0086 M1) --------------

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
            owned.linear_kind(&Type::option(Type::Map(Box::new(Type::Str), Box::new(txn)))),
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
