//! The **program-level tables** the ownership passes read, and the checker's
//! own answer about the type of a node (RFC-0089 M2; RFC-0125 §3 M3).
//!
//! [`movecheck`](crate::movecheck) decides whether a value moves and
//! [`own`](crate::own) decides how a binding is released, and both need to know
//! what a program declares: what a named type is, what a callable returns, what
//! a parameter's type is, which names construct, and — for a TYPE — whether it
//! owns heap, what releases it, and whether it carries an obligation. Those are
//! this module's answers. Each is a reading of a DECLARATION, and none of them
//! is a question about an expression.
//!
//! The type of an expression is a question about an expression, and this module
//! used to derive one: 240 lines that read a literal's shape, a `let`
//! annotation, a return type, a variant table and a protocol's declared return,
//! propagated through a scope stack. The checker decides the same fact for
//! every node and writes it down ([`crate::checker::Recorded`]), so there were
//! two derivations of one thing, and they disagreed — see [`Declared::rec`] for
//! the three ways. [`Declared::type_of`] is a lookup in the checker's record
//! now, and there is nothing behind it.
//!
//! **`None` still means what it always meant.** `own.rs` treats it as "do not
//! release", so an unnamed type leaks, which is always safe; the move check
//! treats it as "does not move", which is the same direction.

use std::collections::HashMap;

use crate::ast::*;
use crate::own::{DropKind, Linear};

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

/// A lexical scope stack, innermost binding wins.
///
/// The rule that makes it correct is that EVERY binder is recorded, including a
/// loop variable, a pattern binder or a lambda parameter that carries no
/// interesting fact of its own. Recording only the interesting ones lets an
/// inner binding inherit an outer binding's property — which is how a `let s = 1`
/// under a `let s = "x"` came to be classified as a String and freed.
pub struct Scopes<T>(Vec<HashMap<String, T>>);

impl<T> Scopes<T> {
    pub fn new(outermost: HashMap<String, T>) -> Self {
        Scopes(vec![outermost])
    }

    pub fn enter(&mut self) {
        self.0.push(HashMap::new());
    }

    pub fn exit(&mut self) {
        self.0.pop();
    }

    pub fn bind(&mut self, name: &str, value: T) {
        self.0.last_mut().unwrap().insert(name.to_string(), value);
    }

    /// Every frame, outermost first, for a walk over all bindings.
    pub fn frames(&self) -> &[HashMap<String, T>] {
        &self.0
    }

    /// The innermost binding of `name`, or `None` if nothing binds it.
    pub fn get(&self, name: &str) -> Option<&T> {
        self.0.iter().rev().find_map(|frame| frame.get(name))
    }

    /// Replace the innermost binding of `name`, leaving the frame it lives in
    /// alone. `bind` would shadow it in the CURRENT frame instead, which an
    /// assignment inside an `if` must not do.
    pub fn rebind(&mut self, name: &str, value: T) {
        if let Some(frame) = self
            .0
            .iter_mut()
            .rev()
            .find(|frame| frame.contains_key(name))
        {
            frame.insert(name.to_string(), value);
        }
    }

    /// Drop every frame above the first `n`, so one outermost frame (module
    /// state, say) is built once and reused for every function body.
    pub fn truncate(&mut self, n: usize) {
        self.0.truncate(n);
    }
}

/// The program-level tables the declared-types reading needs: what each named
/// type is, what each callable returns, and what each module-state binding holds.
///
/// Built once per program. A per-body [`Scopes`] carries the rest.
pub struct Declared {
    /// **What the checker decided about every node of this program**, where a
    /// caller has one to give (RFC-0125 §3 M3, the type slice).
    ///
    /// The checker types every expression and [`crate::checker::Recorded`]
    /// writes the answer down against the node's address. This module was a
    /// SECOND derivation of the same fact from the declarations alone, and the
    /// two disagreed in three recorded ways: a pattern binder and a loop
    /// binder's field had no type here, so `x.copy()` on one read as a copied
    /// handle and its release stood down; an array literal is refused a type
    /// here on purpose, because `ArrayN`, `Array<T>` and `SmallArray<T, N>`
    /// share one syntax and only an annotation tells them apart — which the
    /// checker HAS at every literal and this reading does not; and a name
    /// resolved here by name alone reached across a module boundary to
    /// `examples/shadowbuiltin.vyrn`'s own `fn raw`. An address cannot be
    /// resolved to the wrong declaration, and the checker already solved the
    /// other two.
    ///
    /// `None` where no record exists — a caller that asks about a program the
    /// checker never saw. Nothing else is left of the second derivation.
    rec: Option<std::rc::Rc<crate::checker::Recorded>>,
    /// Every `type X = ..`, so a nominal type answers as its base does.
    decls: HashMap<String, TypeDecl>,
    /// The declared return type of every callable this can name: the program's
    /// functions plus the seeded builtins. A user declaration wins, though
    /// `checker::RESERVED` already forbids taking one of these names.
    ///
    /// The builtin half was `builtin_returns`, four hand-written rows that said
    /// what a call gives back — beside eighteen seeded signatures each already
    /// carrying a `ret`. It was the last of RFC-0094's second lists, and the
    /// two forks had drifted: the list said `@push` returns `Array<Unit>` and
    /// the row says `Array<T>`, which release the same way and read differently.
    /// [`crate::prelude::returns`] is the one answer now.
    ///
    /// It still under-approximates. A builtin whose row is held back has no
    /// type this reading can name, so a binding to it is left alone.
    rets: HashMap<String, Type>,
    /// Declared parameter types per user function — what types an argument
    /// whose own expression cannot answer (an array literal coerced at the
    /// call boundary, RFC-0114 §25's heapify row).
    params: HashMap<String, Vec<Type>>,
    /// Module state (RFC-0013), with its declared type where it has one. Seeded
    /// into the outermost scope frame by a pass that wants globals typed.
    globals: HashMap<String, Option<Type>>,
    /// The `Owned` table, so a pass can ask what a type RELEASES and not only
    /// what it reaches. See [`Declared::releases`].
    owned: crate::declared::Owned,
    /// Every enum variant name the program declares, plus the built-in sum
    /// constructors, each mapped to the enum type it constructs — or `None`
    /// where no single named type answers (the built-in sum constructors,
    /// whose payload parameter this pass never solves; a variant name two
    /// enums share; a generic enum, whose bare name would be an incomplete
    /// type). See [`Declared::constructs`] and the `type_of` Call arm.
    variants: HashMap<String, Option<String>>,
}

impl Declared {
    pub fn new(program: &Program) -> Self {
        let mut rets: HashMap<String, Type> = HashMap::new();
        for (n, t) in crate::prelude::returns() {
            rets.insert(n.to_string(), t.clone());
        }
        let mut params: HashMap<String, Vec<Type>> = HashMap::new();
        for f in &program.functions {
            rets.insert(f.name.clone(), f.ret.clone());
            params.insert(
                f.name.clone(),
                f.params.iter().map(|p| p.ty.clone()).collect(),
            );
        }
        // A protocol method call reaches every pass under its SURFACE name —
        // `n.show()` is `show(n)` — because the impl is selected by the
        // receiver's type, and this pass never selects impls. The protocol's
        // declared return is the one thing every impl agrees on, so a CONCRETE
        // return seeds a row under the surface name. Without it the call typed
        // as unknown, no argument-temporary row was minted, and
        // `print(n.show())` leaked one rendered String per call (exit-residue
        // round thirty-five). A return that mentions a type variable (an
        // associated type arrives as `Type::Param`) is impl-dependent and
        // stays unnamed, and a projection member's result is a PLACE inside
        // the receiver — not rule 3's owned result — so it stays out too.
        for p in &program.protocols {
            for m in &p.methods {
                if m.result_cap.is_none() && !crate::types::mentions_param(&m.ret) {
                    rets.entry(m.name.clone()).or_insert_with(|| m.ret.clone());
                }
            }
        }
        let decls = crate::types::decl_map(program);
        let mut variants: HashMap<String, Option<String>> =
            ["Some", "Ok", "Err", "Success", "Failure"]
                .into_iter()
                .map(|n| (n.to_string(), None))
                .collect();
        for d in decls.values() {
            if let Some(vs) = crate::types::declared_variants(&d.base) {
                for v in vs {
                    // A generic enum's bare name is an incomplete type, and a
                    // variant two enums share names neither — both answer
                    // `None`, which `type_of` reads as "constructs, but the
                    // type is not this pass's to name".
                    let owner = (d.type_params.is_empty() && !variants.contains_key(&v.name))
                        .then(|| d.name.clone());
                    variants.insert(v.name.clone(), owner);
                }
            }
        }
        Declared {
            rec: None,
            owned: crate::declared::Owned::new(program),
            variants,
            decls,
            rets,
            params,
            globals: program
                .globals
                .iter()
                .map(|g| (g.name.clone(), g.ty.clone()))
                .collect(),
        }
    }

    /// Read the types off the checker's own record rather than deriving them
    /// again — see the [`Declared::rec`] field.
    ///
    /// The caller supplies the record because the caller knows which program is
    /// in hand and whether one is held for it: [`crate::checker::recorded`]
    /// serves the analysis's own check where the analysis made one, and checks
    /// once where it did not.
    pub fn recording(mut self, rec: std::rc::Rc<crate::checker::Recorded>) -> Self {
        self.rec = Some(rec);
        self
    }

    /// The type declarations, for a caller that asks `crate::types` directly.
    pub fn decls(&self) -> &HashMap<String, TypeDecl> {
        &self.decls
    }

    /// Module state and its declared type, as a scope frame.
    pub fn globals(&self) -> HashMap<String, Option<Type>> {
        self.globals.clone()
    }

    /// Whether a value of `ty` transitively owns heap — RFC-0089 rule 1, through
    /// the one implementation in [`crate::declared::owns_heap`].
    pub fn owns_heap(&self, ty: &Type) -> bool {
        crate::declared::owns_heap(ty, &self.decls)
    }

    /// Whether `ty` carries a must-use obligation, and which row says so — the
    /// opt-in linear rows (RFC-0086 M3), through the one implementation in
    /// [`crate::declared::Owned::linear_kind`]. A seeded `Stream` and an
    /// `impl MustUse for T` are read out of the same table, so nothing in the
    /// compiler asks this question twice.
    pub fn linear_kind(&self, ty: &Type) -> Option<crate::own::Linear> {
        self.owned.linear_kind(ty)
    }

    /// Whether a value of `ty` is **released** by whoever holds it — the `Owned`
    /// table's own question (RFC-0086 M1), not the transitive one.
    ///
    /// `owns_heap` and this are different questions and Phase 4c needs both. A
    /// record of Strings owns heap and releases nothing, so handing one out
    /// costs a leak; an `Array<T>` releases its buffer, so handing out somebody
    /// else's is a use-after-free.
    pub fn releases(&self, ty: &Type) -> bool {
        self.release_kind(ty).is_some()
    }

    /// [`Declared::releases`] with the row kept: HOW a value of `ty` is
    /// reclaimed, for a caller that has to emit the reclamation and not only
    /// decide it.
    pub fn release_kind(&self, ty: &Type) -> Option<crate::own::DropKind> {
        self.owned.release_kind(ty)
    }

    /// Whether `name` CONSTRUCTS a sum value out of its arguments.
    ///
    /// A variant constructor reads like a call and behaves like a literal: the
    /// value it builds holds the argument and outlives the call. Phase 4c needs
    /// to know, because a payload the caller still names would otherwise be
    /// released while the constructed value holds it — `JArr(out)` handed a
    /// freed buffer to its caller and the walk over it never terminated.
    pub fn constructs(&self, name: &str) -> bool {
        self.variants.contains_key(name)
    }

    /// The element type of an iterable, where this reading can name it.
    ///
    /// `for x in xs` binds an element, so without this every loop variable is
    /// unknown and everything read out of one is unknown after it.
    pub fn elem_of(&self, ty: &Type) -> Option<Type> {
        match crate::types::resolve(ty, &self.decls) {
            Type::Array(t) | Type::ArrayN(t, _) | Type::SmallArray(t, _) | Type::Stream(t) => {
                Some(*t)
            }
            _ => None,
        }
    }

    /// The type of `e` under `vars`, where this reading can name it.
    ///
    /// It is a *declared-types* pass, not a re-run of the checker: parameters,
    /// `let` annotations, function return types and literal shapes, propagated
    /// through the scope stack. When unsure it answers `None`.
    /// The declared type of `callee`'s parameter `ix`, for an argument whose
    /// own expression cannot answer (see `params` above).
    pub fn param_ty(&self, callee: &str, ix: usize) -> Option<&Type> {
        self.params.get(callee).and_then(|ps| ps.get(ix))
    }

    /// Whether `name` is a callable this table knows the return of — a user
    /// function or a seeded builtin row. What the answer buys is rule 3:
    /// such a function's owned result contains none of its read arguments.
    pub fn is_function(&self, name: &str) -> bool {
        self.rets.contains_key(name)
    }

    /// See [`crate::declared::Owned::reaches_declared`].
    pub fn reaches_declared(&self, ty: &Type) -> bool {
        self.owned.reaches_declared(ty)
    }

    /// The type of `e` — **the checker's**, read off its record.
    ///
    /// There is no second derivation behind this. It was 240 lines of one:
    /// a literal's shape, a `let` annotation, a return type, a variant table,
    /// a protocol's declared return, propagated through the scope stack — and
    /// it disagreed with the checker in three recorded ways (see the
    /// [`Declared::rec`] field). What the record does not hold, this does not
    /// answer, and a reader takes `None` for what it has always taken it for.
    pub fn type_of(&self, e: &Expr) -> Option<Type> {
        self.rec
            .as_ref()?
            .node_types
            .get(&(e as *const Expr as usize))
            .cloned()
    }
}

/// The capability map [`crate::movecheck::arg_verdict`] answers a position under: a declared
/// function's parameters, and a protocol method's over them (a method call
/// reaches this pass under its SURFACE name, and the protocol is what both
/// sides agreed on).
///
/// A reading of a DECLARATION and nothing else, so it lives with the other
/// program-level tables rather than in the pass that used to own it: two
/// passes state the same rule at the same position (RFC-0125 §3 M3, the
/// argument slice) — `movecheck::arg_verdict` and the core at the call it
/// lowers — and both read the position out of this one map.
pub fn arg_caps(program: &Program) -> HashMap<String, Vec<Capability>> {
    let mut caps: HashMap<String, Vec<Capability>> = program
        .functions
        .iter()
        .map(|f| {
            (
                f.name.clone(),
                f.params.iter().map(|p| p.capability).collect(),
            )
        })
        .collect();
    for p in &program.protocols {
        for m in &p.methods {
            let mut cs = vec![m.recv];
            cs.extend(m.param_caps.iter().copied());
            caps.insert(m.name.clone(), cs);
        }
    }
    caps
}

/// The capability of one position: the declaration's word where there is one,
/// the seeded row's otherwise, and `None` where neither answers — which is
/// [`crate::movecheck::ArgVerdict::Unknown`] and frees nothing.
pub fn arg_cap(
    caps: &HashMap<String, Vec<Capability>>,
    callee: &str,
    ix: usize,
) -> Option<Capability> {
    caps.get(callee)
        .and_then(|c| c.get(ix))
        .copied()
        .or_else(|| crate::prelude::capability(callee, ix))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lexer::lex, parser::parse};

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
        let p = parse(lex(src).unwrap()).unwrap();
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
            owns_heap(&Type::Named("Tree".to_string()), &types),
            "a recursive enum owns the boxes its payloads travel in"
        );
        // And a record that reaches itself, which is the other shape `own.rs`
        // documents (`type Node = {{ kids: Array<Node> }}`) — that one owns heap
        // through the array as well, so it answered `true` before; this is the
        // enum shape, where the box IS the only heap.
        assert!(
            !owns_heap(&Type::Int, &types),
            "an integer owns nothing, and the cycle rule must not change that"
        );
    }
}
