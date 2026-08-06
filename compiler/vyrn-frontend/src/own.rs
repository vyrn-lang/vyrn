//! Drop **emission** for owned bindings (RFC-0089 rule 4, Phase 4c).
//!
//! This is the *ownership* half of the memory model's Path A — the counterpart
//! to `region` arenas. It decides, per function, three things:
//!
//!   * **droppable** `let` bindings — ones that still own their value where
//!     their block ends, so the backend releases them there; and
//!   * whether the function **transfers** its result, which since rule 3 is the
//!     return type and nothing else; and
//!   * which `get`/`set` sites read a **provably fresh** reference, so their
//!     generation check cannot fail and no engine emits it (RFC-0004 §5.3). That
//!     one is a second pass over the finished `droppable` set — see
//!     [`fresh_refs_in`].
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

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::declared::Scopes;
use crate::movecheck::{Gone, LetOwnership};

/// How a droppable binding is reclaimed at block exit.
///
/// Not `Copy`: [`DropKind::Release`] carries the name of the method the type
/// declared, which is the point of RFC-0086 M1.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DropKind {
    /// A dynamic `String` — `free` the buffer (Path A).
    FreeStr,
    /// A generational reference — `release` the cell (Path B).
    ReleaseRef,
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
    Release(String),
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
            // A GENERIC impl head carries no row yet. Its `release` flattens to
            // a generic function, and the drop site below calls the flattened
            // name unmangled — which is a symbol nothing defines. Recording the
            // row would turn a missing feature into a linker error at the end
            // of a build. Monomorphizing a declared release is the work this
            // waits on; `Slots<T>` is what wants it.
            .filter(|i| i.type_params.is_empty())
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
            types: crate::types::decl_map(program),
        }
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
            return Some(DropKind::Release(f.clone()));
        }
        match crate::types::resolve(ty, &self.types) {
            // ---- the seeded built-in rows ----------------------------------
            Type::Str => Some(DropKind::FreeStr),
            Type::Ref(_) => Some(DropKind::ReleaseRef),
            Type::Array(_) => Some(DropKind::FreeArr),
            Type::SmallArray(..) => Some(DropKind::FreeSmallArr),
            Type::Map(..) => Some(DropKind::FreeMap),
            // A `Stream<T>` is reclaimed too, but through the stream lowering
            // (RFC-0075 M2b), which pushes its own release frame at the binding
            // that produces it. Answering here as well would release it twice.
            Type::Stream(_) => None,
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
            // A **record** and a **user enum** are not on the list, and Phase 5
            // measured why rather than assuming either way.
            //
            // Both hand their insides out as PROJECTIONS, and rule 3 records a
            // returned projection as a LEND rather than refusing it.
            // `check_return` says in its own words why: refusing one would demand
            // `.copy()` from `Json` and `Html`, which refer to themselves and have
            // no structural copy (M1b). Its note ends "so nothing releases them".
            // A row here is exactly what releases them, and three parity runs
            // said so within a minute of each other:
            //
            //   * `std/jsondec`'s `tagOf(v)` is `match v { JStr(s) => s, .. }` —
            //     a `String` the `Json` still holds. The decoder read a freed
            //     `note` field (`examples/jsondecbytes.vyrn`).
            //   * `std/graphql`'s `gqlScanner(src)` returns a RECORD holding
            //     `bytes(src)`, a view of the argument's buffer. Nothing sees
            //     that lend at all: `returned_borrow` reads a returned place and
            //     a struct literal is not one. Releasing the scanner freed the
            //     source String through the wrong pointer (`examples/graphql.vyrn`).
            //   * `gqlParseQuery` writes `GqlQuery { sels: set.sels }` — a field
            //     read stored into a literal, which `store` allows and does not
            //     count as a move, so two records name one buffer.
            //
            // The lend is recorded per `let` and not through a store or through a
            // container, so the analysis cannot answer this today. RFC-0091 M1's
            // `Copy` protocol and 7a's place projections are the two mechanisms
            // that make the wider rule sayable — the same two `check_return`
            // already names.
            //
            // A fixed `[N x T]` is off for census U4's reason: releasing an
            // element would free a `m.keys()` snapshot's pointers twice. A `Fn`
            // is off for the reason `owns_heap` records. A `Task<T>` is a handle
            // to a frame the join owns, and `lazy T` IS `fn() -> T` (RFC-0085
            // M4a) — `resolve` normally answers that, and this is the
            // depth-limited fallback.
            Type::Record(_)
            | Type::Enum(_)
            | Type::ArrayN(..)
            | Type::Fn(..)
            | Type::Task(_)
            | Type::Lazy(_) => None,
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
    fn go(
        ty: &Type,
        types: &HashMap<String, TypeDecl>,
        seen: &mut Vec<String>,
    ) -> Option<String> {
        if let Type::Named(n) | Type::App(n, _) = ty {
            if seen.iter().any(|s| s == n) {
                return Some(n.clone());
            }
            // Not a declared name: nothing to expand, so nothing to recur into.
            if !types.contains_key(n) {
                return None;
            }
            seen.push(n.clone());
            let r = go(&crate::types::resolve(ty, types), types, seen);
            seen.pop();
            return r;
        }
        let mut deeper = |t: &Type| go(t, types, seen);
        match ty {
            Type::Option(t)
            | Type::Array(t)
            | Type::ArrayN(t, _)
            | Type::SmallArray(t, _)
            | Type::Lazy(t)
            | Type::Task(t)
            | Type::Stream(t)
            | Type::Ref(t) => deeper(t),
            Type::Result(a, b) | Type::Map(a, b) => deeper(a).or_else(|| deeper(b)),
            Type::Record(fs) => fs.iter().find_map(|f| go(&f.ty, types, seen)),
            Type::Enum(vs) => vs
                .iter()
                .find_map(|v| v.payload.iter().find_map(|p| go(p, types, seen))),
            _ => None,
        }
    }
    go(ty, types, &mut Vec::new())
}

/// Whether a value of `ty` carries a **must-use** obligation: it has to be
/// consumed by name, and letting it go out of scope is an error.
///
/// This is the row RFC-0090's downsides list promised. Ownership is affine —
/// under RFC-0089 rule 1 a value you stop using is simply released, and that
/// alone would have deleted RFC-0075's "a stream must be consumed" diagnostic
/// the day rule 1 landed. A must-use type is the opt-in linear case: releasing
/// its memory is not the same thing as discharging it, because its producer has
/// a teardown no memory drop can run.
///
/// One row today, `Stream`, reached through the alias resolver so a
/// `type Events = Stream<Event>` carries the obligation its base does. A user
/// type joins the table when it can declare the obligation (RFC-0091 M1's
/// protocol work), not before: an inferred must-use would be exactly the
/// guessing this whole arc removes.
pub fn must_use(ty: &Type, types: &HashMap<String, TypeDecl>) -> bool {
    matches!(crate::types::resolve(ty, types), Type::Stream(_))
}

/// Whether a value of `ty` transitively owns heap, under RFC-0089 rule 1.
///
/// [`Owned::release_kind`] answers about the value's OWN storage; this asks
/// about everything it reaches, because a record of Strings moves under rule 1
/// even though releasing the record releases nothing today. `Ref<T>` is excluded
/// on purpose: RFC-0089 §5 keeps it a freely copied handle, so `r.copy()` shares
/// the cell rather than duplicating it.
///
/// The depth limit is the same guard the rest of this file uses against a
/// declaration that refers to itself; a type that deep is answered `false`, which
/// costs a copy that copies nothing and never a wrong free.
pub fn owns_heap(ty: &Type, types: &HashMap<String, TypeDecl>) -> bool {
    fn go(ty: &Type, types: &HashMap<String, TypeDecl>, depth: usize) -> bool {
        if depth > 8 {
            return false;
        }
        let deeper = |t: &Type| go(t, types, depth + 1);
        match crate::types::resolve(ty, types) {
            Type::Str | Type::Array(_) | Type::SmallArray(..) | Type::Map(..) | Type::Stream(_) => {
                true
            }
            Type::Option(t) | Type::ArrayN(t, _) | Type::Lazy(t) | Type::Task(t) => deeper(&t),
            Type::Result(a, b) => deeper(&a) || deeper(&b),
            Type::Record(fs) => fs.iter().any(|f| deeper(&f.ty)),
            Type::Enum(vs) => vs.iter().any(|v| v.payload.iter().any(&deeper)),
            // A stored function value (RFC-0037) is `{ tag, captures }` and the
            // capture block IS heap — one `malloc` per evaluation of the lambda,
            // which is census §16. It answers `false` anyway, and Phase 5
            // measured the price of the honest answer before writing this down.
            //
            // Saying `true` makes a `Fn` move under rule 1, which is the only
            // thing that would let rule 4 release it: a value that copies freely
            // is two names for one block and the release runs twice. The corpus
            // copies them. `std/http`'s `httpCopy` rebuilds a `Route` thirteen
            // fields at a time and hands `run` and `whole` straight across, which
            // is where seven combinators get their new route; `std/ui` and
            // `examples/rest` do the same. Under rule 1 every one of those is a
            // store of a borrowed `fn`, and the fix menu's second entry —
            // `.copy()` — cannot be written: a capture block's layout is per
            // TAG, chosen at run time, so a structural copy has nothing to
            // measure.
            //
            // **RFC-0091 M1 was named as the mechanism and is not it.** M1
            // landed, and it keys a `Copy` row by a TYPE KEY. A `fn` type is
            // structural and has none, and a `type Bump = fn(..) -> ..` alias
            // over one is refused where it is written: the value erases at run
            // time and carries no name to dispatch on. So §16 has nowhere to
            // hang a declaration, and nothing to write in it either — the tags
            // are the defunctionalizer's and have no source name.
            //
            // What it waits on is a copy DERIVED over the defunctionalized enum,
            // emitted where RFC-0037 already emits that enum, which knows every
            // tag's layout because it chose them. That is a job in the closure
            // lowering, not a row in a protocol table.
            Type::Fn(..) => false,
            _ => false,
        }
    }
    go(ty, types, 0)
}

/// Why a binding is **not** reclaimed at block exit (RFC-0087 U1).
///
/// Three of the rows come straight from [`crate::movecheck`], which decided them
/// while it was enforcing rules 1 to 3. The other two are this file's, because
/// neither is about the value: an arena owns what is allocated inside it, and a
/// literal was never allocated.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Leak {
    /// The type releases nothing. Carries the type, or `unknown`.
    NoRelease(String),
    /// The binding names storage somebody else owns (rule 2). Carries what it
    /// is, in words.
    Borrowed(&'static str),
    /// `mut`, and reclaimed by something the engines can observe — see the
    /// [`Fate`] decision for why those two cannot yet move together.
    Mutable,
    /// Lexically inside a `region` — the arena owns it.
    Region,
    /// A lambda or a `spawn` holds it, and either can outlive this block.
    Captured { line: usize },
    /// A second name reads it without taking it, so neither name is the owner.
    Aliased { line: usize },
    /// It reached a call that may retain it.
    Escaped { callee: String, line: usize },
}

impl Leak {
    /// The reason with its lines and names removed, so a corpus of them groups.
    pub fn kind(&self) -> &'static str {
        match self {
            Leak::NoRelease(_) => "the type owns no heap",
            Leak::Borrowed(_) => "it names somebody else's value",
            Leak::Mutable => "`mut`, and observably reclaimed",
            Leak::Region => "inside a `region`",
            Leak::Captured { .. } => "captured by a lambda or a spawn",
            Leak::Aliased { .. } => "aliased by another binding",
            Leak::Escaped { .. } => "escaped into a call",
        }
    }
}

impl std::fmt::Display for Leak {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Leak::NoRelease(ty) => write!(f, "the type {ty} owns no heap"),
            Leak::Borrowed(what) => write!(f, "it is {what}"),
            Leak::Mutable => write!(f, "it is `mut`, and its release is observable"),
            Leak::Region => write!(f, "it is inside a `region` — the arena owns it"),
            Leak::Captured { line } => {
                write!(f, "a lambda or a spawn captures it at line {line}")
            }
            Leak::Aliased { line } => write!(f, "another binding aliases it at line {line}"),
            Leak::Escaped { callee, line } => {
                write!(f, "it escapes into the call to `{callee}` at line {line}")
            }
        }
    }
}

/// What happens to a `let` binding's value at the end of its block.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Fate {
    /// The engines release it here, this way.
    Reclaimed(DropKind),
    /// It left: a `return` carried it out, or a store took it. Whoever holds it
    /// now reclaims it, so this block must not.
    Moved { line: usize, into: String },
    /// `drop name` reclaims it, so the automatic path must not.
    Dropped { line: usize },
    /// It is static data in the module's data segment. Nothing reclaims it,
    /// and nothing needs to (census §1).
    Static,
    /// Nothing reclaims it.
    Leaked(Leak),
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
    /// Per function: every `let` in source order, and what happens to its value
    /// — the same decisions `droppable` carries, plus the reason for each one
    /// this analysis did NOT take. Recorded by the walker that decides, so the
    /// report and the emission cannot disagree (RFC-0087 U1).
    pub notes: HashMap<String, Vec<BindingNote>>,
    /// The `Ref` argument of every `get`/`set` whose generation check cannot
    /// fail, keyed by that argument expression's node address — RFC-0004 §5.3.
    /// See [`fresh_refs_in`] for the condition. Flat across the program, because
    /// node addresses are already unique.
    pub fresh_refs: HashSet<usize>,
    /// The `Owned` table this analysis decided with. Handed out so a backend
    /// lowering an explicit `drop x` asks the SAME question the automatic path
    /// asked, instead of keeping a second copy of the answer.
    pub proto: Owned,
}

/// Analyse ownership across a whole program.
pub fn analyze(program: &Program) -> Ownership {
    let proto = Owned::new(program);
    // What every `let` in the program still owns where its block ends, decided
    // by the pass that enforces the rules. One walk, one answer, no second
    // opinion (RFC-0087 records three defects that were two walkers disagreeing).
    let lets = crate::movecheck::ownership(program);

    let mut droppable = HashMap::new();
    let mut notes = HashMap::new();
    let mut fresh_refs = HashSet::new();
    let mut emit = |name: String, body: &Block| {
        let r = emit_body(body, &lets, &proto);
        fresh_refs.extend(fresh_refs_in(body, &r.droppable));
        droppable.insert(name.clone(), r.droppable);
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
        notes,
        fresh_refs,
        proto,
    }
}

struct FnResult {
    droppable: HashMap<usize, DropKind>,
    notes: Vec<BindingNote>,
}

/// One body's drop sites, in source order.
fn emit_body(
    body: &Block,
    lets: &HashMap<usize, LetOwnership>,
    proto: &Owned,
) -> FnResult {
    let mut e = Emit {
        droppable: HashMap::new(),
        notes: Vec::new(),
        region_depth: 0,
        lets,
        proto,
    };
    e.block(body);
    FnResult {
        droppable: e.droppable,
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
                name,
                mutable,
                value,
                line,
                ..
            } => {
                let fate = self.fate(s, *mutable, value);
                if let Fate::Reclaimed(kind) = &fate {
                    self.droppable.insert(id(s), kind.clone());
                }
                self.notes.push(BindingNote {
                    name: name.clone(),
                    line: *line,
                    fate,
                });
            }
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
                self.block(then_block);
                if let Some(eb) = else_block {
                    self.block(eb);
                }
            }
            Stmt::While { body, .. } | Stmt::ForIn { body, .. } => self.block(body),
            Stmt::Region { body, .. } => {
                self.region_depth += 1;
                self.block(body);
                self.region_depth -= 1;
            }
            // A lambda's block body carries `let`s of its own, and the engines
            // walk it as part of this function's AST.
            Stmt::Expr(e) => self.lambdas(e),
            Stmt::Assign { value, .. }
            | Stmt::SetField { value, .. }
            | Stmt::IndexSet { value, .. } => self.lambdas(value),
            Stmt::Return { value, .. } => {
                if let Some(e) = value {
                    self.lambdas(e);
                }
            }
            Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Drop { .. } => {}
        }
    }

    /// Walk into any lambda block body inside `e`, so its `let`s get rows too.
    ///
    /// Only the statement-carrying case needs the descent: an expression-bodied
    /// lambda has no `let` to reclaim.
    fn lambdas(&mut self, e: &Expr) {
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
            | Expr::Field { expr: inner, .. } => self.lambdas(inner),
            Expr::Binary { lhs, rhs, .. } => {
                self.lambdas(lhs);
                self.lambdas(rhs);
            }
            Expr::Call { args, .. }
            | Expr::TryConstruct { args, .. }
            | Expr::ArrayLit { elems: args, .. }
            | Expr::Spawn { args, .. } => {
                for a in args {
                    self.lambdas(a);
                }
            }
            Expr::StructLit { fields, .. } => {
                for (_, v) in fields {
                    self.lambdas(v);
                }
            }
            Expr::MapLit { entries, .. } => {
                for (k, v) in entries {
                    self.lambdas(k);
                    self.lambdas(v);
                }
            }
            Expr::Match { scrutinee, arms, .. } => {
                self.lambdas(scrutinee);
                for a in arms {
                    self.lambdas(&a.body);
                }
            }
            Expr::IfExpr {
                cond,
                then_branch,
                else_branch,
                ..
            } => {
                self.lambdas(cond);
                self.lambdas(then_branch);
                if let Some(eb) = else_branch {
                    self.lambdas(eb);
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

    /// What happens to one `let` binding's value — RFC-0089 rule 4, in full.
    ///
    /// The order of the questions is the order a reader needs them. What does
    /// the TYPE release? Nothing, and there is nothing more to say. Something,
    /// and then: does anything else own this storage?
    fn fate(&self, s: &Stmt, mutable: bool, value: &Expr) -> Fate {
        let row = self.lets.get(&id(s));
        let bty = row.and_then(|r| r.ty.as_ref());
        let Some(kind) = bty.and_then(|t| self.proto.release_kind(t)) else {
            return Fate::Leaked(Leak::NoRelease(match bty {
                Some(t) => t.to_string(),
                None => "unknown".into(),
            }));
        };
        // A string literal lives in the data segment. Nothing allocated it and
        // nothing reclaims it (census §1).
        if matches!(value, Expr::Str(_)) {
            return Fate::Static;
        }
        // A dynamic string inside a region is the arena's, and the two
        // mechanisms partition every allocation — nothing is freed twice. A cell
        // (`ReleaseRef`) lives in the separate slab, which the region does not
        // touch, so it is released regardless.
        if kind == DropKind::FreeStr && self.region_depth > 0 {
            return Fate::Leaked(Leak::Region);
        }
        // A `mut` binding is released by its slot's FINAL value in both
        // compiling backends and by the value captured at the `let` in the
        // interpreter. For a `free` that is the same program — neither engine
        // can see the difference. For a cell release and for a user type's
        // `release`, which prints, it is not, so those two wait for a phase that
        // makes the interpreter read the slot.
        if mutable && matches!(kind, DropKind::ReleaseRef | DropKind::Release(_)) {
            return Fate::Leaked(Leak::Mutable);
        }
        match row.and_then(|r| r.gone.as_ref()) {
            None => Fate::Reclaimed(kind),
            Some(Gone::Borrowed(what)) => Fate::Leaked(Leak::Borrowed(what)),
            Some(Gone::Aliased { line }) => Fate::Leaked(Leak::Aliased { line: *line }),
            Some(Gone::Lent { line, to }) => Fate::Leaked(Leak::Escaped {
                callee: to.clone(),
                line: *line,
            }),
            Some(Gone::Captured { line }) => Fate::Leaked(Leak::Captured { line: *line }),
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

/// The `get`/`set` sites in `body` whose generation check can never fail
/// (RFC-0004 §5), given that function's *final* `droppable` set.
///
/// A `let c = cell(..)` survives into `droppable` with [`DropKind::ReleaseRef`]
/// only if nothing aliased it, nothing was handed it, and no `release(c)`
/// reached it — `release` is deliberately outside the safe-read list in
/// [`Analysis::visit`], so an explicit release removes the binding. The one
/// release left is the compiler's own at block exit, which runs after every
/// access in the block. So the reference a `get(c)`/`set(c, ..)` reads is the
/// one `cell(..)` just handed out and its generation is the slot's: the check
/// has one possible answer.
///
/// This has to be a second pass. `droppable` is order-independent only once it
/// is final — a `get(c)` can precede the `release(c)` that escapes `c`.
///
/// The key is the *argument* expression's node address, which is what all three
/// engines hold at their check site.
fn fresh_refs_in(body: &Block, droppable: &HashMap<usize, DropKind>) -> HashSet<usize> {
    let mut f = Fresh {
        droppable,
        scopes: Scopes::new(HashMap::new()),
        out: HashSet::new(),
    };
    f.block(body);
    f.out
}

struct Fresh<'a> {
    droppable: &'a HashMap<usize, DropKind>,
    /// Scope stack of every name in scope: a `let` maps to its node identity, a
    /// pattern or loop binder to 0. Parameters are absent. Only a `let` can name
    /// a droppable cell, so 0 and "absent" both answer "not fresh" — but the
    /// binder still has to be *recorded*, or a `for c in refs` inside a block
    /// that also has `let c = cell(..)` would resolve to the wrong one.
    scopes: Scopes<usize>,
    out: HashSet<usize>,
}

impl Fresh<'_> {
    fn block(&mut self, b: &Block) {
        self.scopes.enter();
        for s in &b.stmts {
            self.stmt(s);
        }
        self.scopes.exit();
    }

    /// Run `body` with `binders` in scope ahead of it (an `if let` arm, a `for`).
    fn scoped(&mut self, binders: &[String], body: &Block) {
        self.scopes.enter();
        for n in binders {
            self.scopes.bind(n, 0);
        }
        self.block(body);
        self.scopes.exit();
    }

    /// Whether `name` is bound by a `let` that owns a cell nothing else can reach.
    fn is_fresh_cell(&self, name: &str) -> bool {
        self.scopes
            .get(name)
            .is_some_and(|key| self.droppable.get(key) == Some(&DropKind::ReleaseRef))
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Let { name, value, .. } => {
                self.expr(value);
                self.scopes.bind(name, id(s));
            }
            Stmt::Assign { value, .. } | Stmt::SetField { value, .. } => self.expr(value),
            Stmt::IndexSet { index, value, .. } => {
                self.expr(index);
                self.expr(value);
            }
            Stmt::Return { value, .. } => {
                if let Some(e) = value {
                    self.expr(e);
                }
            }
            Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Drop { .. } => {}
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                self.expr(cond);
                self.block(then_block);
                if let Some(eb) = else_block {
                    self.block(eb);
                }
            }
            Stmt::IfLet {
                pattern,
                scrutinee,
                then_block,
                else_block,
                ..
            } => {
                self.expr(scrutinee);
                self.scoped(&pattern_binders(pattern), then_block);
                if let Some(eb) = else_block {
                    self.block(eb);
                }
            }
            Stmt::While { cond, body, .. } => {
                self.expr(cond);
                self.block(body);
            }
            Stmt::ForIn {
                var, iter, body, ..
            } => {
                self.expr(iter);
                self.scoped(std::slice::from_ref(var), body);
            }
            Stmt::Expr(e) => self.expr(e),
            Stmt::Region { body, .. } => self.block(body),
        }
    }

    fn expr(&mut self, e: &Expr) {
        match e {
            Expr::Int(_) | Expr::Byte(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => {}
            Expr::Var { .. } => {}
            Expr::Unary { expr, .. } | Expr::Try { expr, .. } | Expr::Field { expr, .. } => {
                self.expr(expr)
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            Expr::Call { name, args, .. } => {
                if (name == "get" || name == "set") && !args.is_empty() {
                    if let Expr::Var { name: c, .. } = &args[0] {
                        if self.is_fresh_cell(c) {
                            self.out.insert(&args[0] as *const Expr as usize);
                        }
                    }
                }
                for a in args {
                    self.expr(a);
                }
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.expr(scrutinee);
                for arm in arms {
                    self.scopes.enter();
                    for n in pattern_binders(&arm.pattern) {
                        self.scopes.bind(&n, 0);
                    }
                    self.expr(&arm.body);
                    self.scopes.exit();
                }
            }
            Expr::IfExpr {
                cond,
                then_branch,
                else_branch,
                ..
            } => {
                self.expr(cond);
                self.expr(then_branch);
                if let Some(eb) = else_branch {
                    self.expr(eb);
                }
            }
            Expr::StructLit { fields, .. } => {
                for (_, v) in fields {
                    self.expr(v);
                }
            }
            Expr::TryConstruct { args, .. }
            | Expr::ArrayLit { elems: args, .. }
            | Expr::Spawn { args, .. } => {
                for a in args {
                    self.expr(a);
                }
            }
            Expr::MapLit { entries, .. } => {
                for (k, v) in entries {
                    self.expr(k);
                    self.expr(v);
                }
            }
            // A lambda body is NOT walked. Its `get(c)` on a captured cell runs
            // whenever the closure runs, and a stored closure (RFC-0037) can run
            // after the block that released `c` has exited. `visit` counts that
            // `get` as a safe read, so `c` stays droppable — the block-exit
            // release is what the check would then catch. Elide nothing there.
            Expr::Lambda { .. } => {}
        }
    }
}

/// The names a refutable pattern binds.
fn pattern_binders(p: &Pattern) -> Vec<String> {
    match p {
        Pattern::Some(n)
        | Pattern::Ok(n)
        | Pattern::Err(n)
        | Pattern::Success(n)
        | Pattern::Failure(n) => vec![n.clone()],
        Pattern::Variant(_, ns) => ns.clone(),
        Pattern::None => Vec::new(),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{lexer::lex, parser::parse};

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
    fn set_value_argument_escapes() {
        // `set(c, s)` stores `s` in the cell, which outlives the block — `s`
        // must NOT stay droppable (auto-freeing it would leave the cell
        // dangling; the next `get` would be a use-after-free).
        let src = "fn main() -> Int64 { let a = \"x\"; let b = \"y\"; \
                   let c = cell(\"seed\"); \
                   if true { let s = a + b; set(c, s); } \
                   print(get(c)); release(c); return 0; }";
        assert_eq!(drop_count(src, "main"), 0);
    }

    #[test]
    fn set_ref_argument_is_a_safe_read() {
        // Passing an owned *cell* to `set`/`get` does not escape the cell
        // binding — with no explicit `release`, it stays auto-releasable.
        let src = "fn main() -> Int64 { let c = cell(1); set(c, 2); \
                   let n = get(c); return n; }";
        assert_eq!(drop_count(src, "main"), 1);
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

    /// The two kinds `mut` still excludes, and why: both compiling engines
    /// release a `mut` slot's FINAL value, and the interpreter releases the value
    /// captured at the `let`. A `free` cannot tell the two apart; a cell release
    /// can, and a declared `release` prints.
    #[test]
    fn a_mutable_cell_is_not_auto_released() {
        let src = "fn main() -> Int64 { let mut c = cell(1); c = cell(2); return get(c); }";
        assert_eq!(drop_count(src, "main"), 0);
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
                   let ns: Array<Int64> = array(); \
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
                   return apply(|s| { let t = s + 1; print(t); return t + 1; }, 2); }";
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

    /// A `Ref<T>` copy shares the cell (RFC-0089 §5), so calling it a transfer
    /// would release one cell twice.
    #[test]
    fn copying_a_ref_is_not_a_transfer() {
        let src = "fn main() -> Int64 { let c = cell(5); let d = c.copy(); \
                   set(d, 7); return get(c); }";
        assert_eq!(drop_count(src, "main"), 1);
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

    #[test]
    fn non_escaping_cell_is_auto_released() {
        let src = "fn main() -> Int64 { let c = cell(1); set(c, get(c) + 1); return get(c); }";
        assert_eq!(drop_kinds(src, "main"), vec![DropKind::ReleaseRef]);
    }

    #[test]
    fn aliased_cell_is_not_auto_released() {
        // `c` is aliased into `d`, so it must not be auto-released.
        let src = "fn main() -> Int64 { let c = cell(1); let d = c; return get(d); }";
        assert_eq!(drop_count(src, "main"), 0);
    }

    #[test]
    fn explicitly_released_cell_is_not_auto_released() {
        // Passing `c` to `release` hands the cell off — no auto-release on top,
        // which would double-release and trap.
        let src = "fn main() -> Int64 { let c = cell(1); let v = get(c); release(c); return v; }";
        assert_eq!(drop_count(src, "main"), 0);
    }

    #[test]
    fn cell_inside_region_is_still_released() {
        // The cell slab is separate from the arena, so a region does not reclaim
        // it — ownership still auto-releases the reference.
        let src = "fn main() -> Int64 { let mut n = 0; \
                   region { let c = cell(7); n = get(c); } return n; }";
        assert_eq!(drop_kinds(src, "main"), vec![DropKind::ReleaseRef]);
    }

    // ---- auto-free for mutable arrays -----------------------------------

    #[test]
    fn mut_array_with_self_update_is_auto_freed() {
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = array(); \
                   let mut i = 0; while i < 3 { a = push(a, i); i = i + 1; } \
                   return at(a, 0); }";
        assert_eq!(drop_kinds(src, "main"), vec![DropKind::FreeArr]);
    }

    #[test]
    fn explicitly_dropped_array_is_not_auto_freed() {
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = array(); \
                   a = push(a, 1); let v = at(a, 0); drop a; return v; }";
        assert_eq!(drop_count(src, "main"), 0);
    }

    #[test]
    fn returned_array_is_not_auto_freed() {
        let src = "fn build() -> Array<Int64> { let mut a: Array<Int64> = array(); \
                   a = push(a, 1); return a; } fn main() -> Int64 { return 0; }";
        // `a` is moved out by the return, so it is not freed inside `build`.
        assert_eq!(drop_count(src, "build"), 0);
    }

    // ---- elided generation checks (RFC-0004 §5) --------------------------

    fn fresh_count(src: &str) -> usize {
        let p = parse(lex(src).unwrap()).unwrap();
        analyze(&p).fresh_refs.len()
    }

    #[test]
    fn accesses_to_a_non_escaping_cell_are_fresh() {
        // `set`, the `get` inside it, and the trailing `get` — three sites.
        let src = "fn main() -> Int64 { let c = cell(1); set(c, get(c) + 1); return get(c); }";
        assert_eq!(fresh_count(src), 3);
    }

    #[test]
    fn an_explicit_release_makes_every_access_checked() {
        // The `release` is textually last, so only the final `droppable` says so.
        let src = "fn main() -> Int64 { let c = cell(1); let v = get(c); release(c); return v; }";
        assert_eq!(fresh_count(src), 0);
    }

    #[test]
    fn an_aliased_cell_stays_checked() {
        let src = "fn main() -> Int64 { let c = cell(1); let d = c; return get(d) + get(c); }";
        assert_eq!(fresh_count(src), 0);
    }

    #[test]
    fn a_parameter_reference_stays_checked() {
        let src = "fn bump(r: Ref<Int64>) -> Int64 { set(r, get(r) + 1); return get(r); } \
                   fn main() -> Int64 { return 0; }";
        assert_eq!(fresh_count(src), 0);
    }

    #[test]
    fn a_loop_binder_does_not_borrow_an_outer_cells_freshness() {
        // `c` inside the loop is the element, not the cell — resolving it to the
        // outer `let` would elide a check on a reference this analysis never saw.
        let src = "fn main() -> Int64 { let c = cell(1); let rs: Array<Ref<Int64>> = array(); \
                   for c in rs { print(get(c)); } return get(c); }";
        assert_eq!(fresh_count(src), 1);
    }

    #[test]
    fn a_captured_cell_is_never_fresh_inside_the_lambda() {
        // The closure can outlive the block that releases `c`.
        let src = "fn apply(f: fn(Int64) -> Int64, x: Int64) -> Int64 { return f(x); } \
                   fn main() -> Int64 { let c = cell(1); return apply(|x| x + get(c), 2); }";
        assert_eq!(fresh_count(src), 0);
    }

    // ---- the type answers, not the expression (RFC-0086 M1) --------------

    #[test]
    fn an_annotated_array_literal_is_released() {
        // The defect the RFC was written from: `Expr::ArrayLit` was absent from
        // the expression list, so this leaked on every engine while the identical
        // `array()` call did not. Nothing forced the two to agree, because the
        // list was what decided.
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = []; \
                   a = push(a, 1); return at(a, 0); }";
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
    fn a_fresh_key_snapshot_is_released() {
        // `m.keys()` copies the key pointers into a new buffer (RFC-0028) and was
        // absent from the same list.
        let src = "fn main() -> Int64 { let m: Map<String, Int64> = [\"a\": 1]; \
                   let ks = m.keys(); return ks.length; }";
        // The map and the snapshot, in whichever order the map iterates.
        let mut kinds = drop_kinds(src, "main");
        kinds.sort_by_key(|k| format!("{k:?}"));
        assert_eq!(kinds, vec![DropKind::FreeArr, DropKind::FreeMap]);
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
            Some(&DropKind::Release("Owned__Ring__release".to_string()))
        );
        assert_eq!(
            drop_kinds(src, "main"),
            vec![DropKind::Release("Owned__Ring__release".to_string())]
        );
    }

    /// A record is still reclaimed by nothing, and Phase 5 kept it that way with
    /// a measurement rather than with the old argument. See
    /// [`Owned::release_kind`] for the three parity failures a row here produced:
    /// a record hands its insides out as projections, and rule 3 records a
    /// returned projection as a lend rather than refusing it.
    #[test]
    fn a_record_is_reclaimed_by_nothing() {
        let src = "type Ring = { slots: Array<Int64> } \
                   fn make() -> Ring { return Ring { slots: [] } } \
                   fn main() -> Int64 { let r = make(); return 0; }";
        let (o, _) = analyze_src(src);
        assert!(!o.owned_fns.contains_key("make"));
        assert_eq!(drop_count(src, "main"), 0);
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
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel);
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
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
            let Ok(src) = std::fs::read_to_string(path) else { continue };
            lines += src.lines().count();
            let Ok(tokens) = crate::lexer::lex(&src) else { continue };
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
        println!("corpus: {} files ({parsed} parsed), {lines} lines", files.len());
        println!("RFC-0089 rule 3 — returns of a parameter: {}", param_returns.len());
        for s in &param_returns {
            println!("    {s}");
        }
        println!("RFC-0089 rule 1 — bare aliases of an owning type: {}", aliases.len());
        for s in &aliases {
            println!("    {s}");
        }
        println!("move surface: {}", param_returns.len() + aliases.len());
        println!("bindings: {total} — {kept} reclaimed/moved/dropped/static, {leaks} not reclaimed");
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
                Stmt::While { body, .. }
                | Stmt::ForIn { body, .. }
                | Stmt::Region { body, .. } => walk_stmts(body, f),
                _ => {}
            }
        }
    }

    #[test]
    fn factory_returning_cell_is_owned() {
        let src =
            "fn make(v: Int64) -> Ref<Int64> { return cell(v); } fn main() -> Int64 { return 0; }";
        let (o, _) = analyze_src(src);
        assert_eq!(o.owned_fns.get("make"), Some(&DropKind::ReleaseRef));
    }
}
