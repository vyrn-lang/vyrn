//! Ownership / drop analysis for heap temporaries (RFC-0004 §4).
//!
//! This is the *ownership* half of the memory model's Path A — the counterpart
//! to `region` arenas. It decides, per function, three things:
//!
//!   * **droppable** `let` bindings — ones that own a fresh heap allocation and
//!     provably do not escape their block, so the backend frees them at block
//!     exit; and
//!   * whether the function **returns an owned value** — every heap-typed return
//!     hands the caller a fresh, unaliased allocation, transferring ownership out
//!     so the *caller's* receiving binding becomes droppable in turn; and
//!   * which `get`/`set` sites read a **provably fresh** reference, so their
//!     generation check cannot fail and no engine emits it (RFC-0004 §5.3). That
//!     one is a second pass over the finished `droppable` set — see
//!     [`fresh_refs_in`].
//!
//! **Two questions, and only one of them is about the expression** (RFC-0086 M1).
//! A binding needs cleanup when its initializer *transfers* a value nobody else
//! holds AND its **type** says how that value is released. Transfer is a property
//! of the expression form — `at(a, 0)` and `m.keys()` have the same `Array` type
//! and opposite answers — and [`Analysis::transfers`] answers it exhaustively, so
//! a new expression form has to. Release is a property of the type, and [`Owned`]
//! is the only place it is answered: seeded built-in rows plus every
//! `impl Owned for T` in the program. There is no second list.
//!
//! A binding is droppable unless it
//! is `mut`, lexically inside a `region` (the arena owns it), or *escapes*: it
//! appears anywhere except as a whole argument of `print`/`@concat` (which only
//! read a string), an operand of a binary operator (`==`/`+`/…, all reads), or
//! `s.length`.
//! Returning a local owner is a *move* (the value leaves, so it is not dropped
//! here); aliasing it (`let t = x`) or passing it to any other function escapes
//! it. Anything not provably single-owned is simply left to leak — always safe,
//! never a use-after-free or double-free.
//!
//! Identities are `Stmt::Let` node addresses (`*const Stmt as usize`): the
//! backend runs this on the same borrowed AST it emits, so the addresses match
//! one-to-one — a collision-free key where a source line is not (two `let`s can
//! share a line). Because a non-region string concat uses `malloc` and a region
//! one uses the arena, and this analysis skips the region case, the two
//! reclamation mechanisms partition every allocation — nothing is freed twice.

use std::collections::{HashMap, HashSet};

use crate::ast::*;

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
            // ---- aggregates whose ELEMENTS are a safe leak (RFC-0011) -------
            // Each of these is a value the engines copy; anything heap inside is
            // owned by whoever produced it. A row here would free a payload the
            // producer still holds, so the honest answer is that they own nothing
            // of their own.
            Type::Option(_)
            | Type::Result(..)
            | Type::Record(_)
            | Type::Enum(_)
            | Type::ArrayN(..)
            | Type::Task(_)
            | Type::Fn(..)
            // `lazy T` IS `fn() -> T` (RFC-0085 M4a); `resolve` normally answers
            // that, and this is the depth-limited fallback.
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

/// The built-in calls that hand the caller a fresh heap value, with the type of
/// that value. **This is a fact about a function, not about a type** — `at(a, 0)`
/// and `m.keys()` both return an element of a container and only one of them
/// allocates — so it cannot be derived from a signature and the compiler knows it
/// intrinsically, exactly as it knows the seeded [`Owned`] rows.
///
/// It under-approximates on purpose. A builtin missing from here leaks, which is
/// always safe; one wrongly present frees memory somebody still holds.
fn builtin_producers() -> impl Iterator<Item = (&'static str, Type)> {
    [
        // `a + b` on Strings, and `"..\{x}"` interpolation.
        ("@concat", Type::Str),
        ("@str", Type::Str),
        ("cell", Type::Ref(Box::new(Type::Unit))),
        ("array", Type::Array(Box::new(Type::Unit))),
        ("push", Type::Array(Box::new(Type::Unit))),
        // `m.keys()` copies the key pointers into a new buffer (RFC-0028).
        ("@keys", Type::Array(Box::new(Type::Str))),
    ]
    .into_iter()
}

/// Whole-program ownership facts.
pub struct Ownership {
    /// Functions whose return value transfers heap ownership to the caller,
    /// with the kind of value returned.
    pub owned_fns: HashMap<String, DropKind>,
    /// Per function: identity of each droppable `let` and how to reclaim it.
    pub droppable: HashMap<String, HashMap<usize, DropKind>>,
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

    // The declared return type of every callable the analysis can name: the
    // program's functions, plus the built-in producers. A user declaration wins,
    // though `checker::RESERVED` already forbids taking one of these names.
    let mut ret_types: HashMap<String, Type> = HashMap::new();
    for (n, t) in builtin_producers() {
        ret_types.insert(n.to_string(), t);
    }
    for f in &program.functions {
        ret_types.insert(f.name.clone(), f.ret.clone());
    }

    // Callables whose result the caller owns. The built-ins are seeded and stay;
    // every heap-returning function is assumed owned and may be removed below.
    let seeded: HashSet<String> = builtin_producers().map(|(n, _)| n.to_string()).collect();
    let mut owned: HashSet<String> = seeded.clone();
    owned.extend(
        program
            .functions
            .iter()
            .filter(|f| proto.release_kind(&f.ret).is_some())
            .map(|f| f.name.clone()),
    );

    // Fixpoint: remove any function that has a non-owned heap return under the
    // current assumptions. Monotone (only shrinks), so it terminates.
    loop {
        let mut changed = false;
        let snapshot = owned.clone();
        for f in &program.functions {
            if snapshot.contains(&f.name)
                && !seeded.contains(&f.name)
                && !analyze_fn(f, &snapshot, &ret_types, &proto).is_owned
            {
                owned.remove(&f.name);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // Final droppable sets, computed under the fixed owned set.
    let mut droppable = HashMap::new();
    let mut fresh_refs = HashSet::new();
    for f in &program.functions {
        let d = analyze_fn(f, &owned, &ret_types, &proto).droppable;
        fresh_refs.extend(fresh_refs_in(&f.body, &d));
        droppable.insert(f.name.clone(), d);
    }
    // Test bodies (RFC-0015) get the same block-exit drop analysis so a `let` in
    // a test reclaims its heap value exactly as it would in a function. The body
    // is the REAL node the interpreter walks, so the by-address droppable keys
    // match at run time. Tests never return an owned value (they are `Unit`).
    for (i, t) in program.tests.iter().enumerate() {
        let d = analyze_body(&[], &t.body, &Type::Unit, &owned, &ret_types, &proto).droppable;
        fresh_refs.extend(fresh_refs_in(&t.body, &d));
        droppable.insert(format!("test@{i}"), d);
    }
    // Bench bodies (RFC-0055) get the same block-exit drop analysis, keyed by the
    // synthetic `bench@<index>` name the interpreter (`--check`) walks.
    for (i, b) in program.benches.iter().enumerate() {
        let d = analyze_body(&[], &b.body, &Type::Unit, &owned, &ret_types, &proto).droppable;
        fresh_refs.extend(fresh_refs_in(&b.body, &d));
        droppable.insert(format!("bench@{i}"), d);
    }
    // The public view is about the PROGRAM's functions: a built-in producer is
    // seeded knowledge, not a fact discovered about a declaration.
    let owned_fns = program
        .functions
        .iter()
        .filter(|f| owned.contains(&f.name))
        .filter_map(|f| proto.release_kind(&f.ret).map(|k| (f.name.clone(), k)))
        .collect();
    Ownership {
        owned_fns,
        droppable,
        fresh_refs,
        proto,
    }
}

struct FnResult {
    droppable: HashMap<usize, DropKind>,
    is_owned: bool,
}

fn analyze_fn(
    f: &Function,
    owned: &HashSet<String>,
    ret_types: &HashMap<String, Type>,
    proto: &Owned,
) -> FnResult {
    analyze_body(&f.params, &f.body, &f.ret, owned, ret_types, proto)
}

/// The core of [`analyze_fn`], parameterized over a body directly so a test body
/// (RFC-0015) — which has no surrounding `Function` node — can be analysed with
/// the SAME node addresses the interpreter walks (a clone would not match).
fn analyze_body(
    params_list: &[Param],
    body: &Block,
    ret: &Type,
    owned: &HashSet<String>,
    ret_types: &HashMap<String, Type>,
    proto: &Owned,
) -> FnResult {
    // Seed the outermost scope with every parameter and its declared type.
    let params: HashMap<String, Option<Type>> = params_list
        .iter()
        .map(|p| (p.name.clone(), Some(p.ty.clone())))
        .collect();
    let mut a = Analysis {
        droppable: HashMap::new(),
        live: vec![HashMap::new()],
        region_depth: 0,
        owned,
        ret_is_heap: proto.release_kind(ret).is_some(),
        all_returns_owned: true,
        ret_types,
        proto,
        var_types: Scopes::new(params),
    };
    a.block(body);
    FnResult {
        droppable: a.droppable,
        is_owned: a.ret_is_heap && a.all_returns_owned,
    }
}

/// The identity key for a statement: its node address.
fn id(s: &Stmt) -> usize {
    s as *const Stmt as usize
}

/// A lexical scope stack, innermost binding wins.
///
/// The rule that makes it correct is that EVERY binder is recorded, including a
/// loop variable, a pattern binder or a lambda parameter that carries no
/// interesting fact of its own. Recording only the interesting ones lets an
/// inner binding inherit an outer binding's property — which is how a `let s = 1`
/// under a `let s = "x"` came to be classified as a String and freed.
struct Scopes<T>(Vec<HashMap<String, T>>);

impl<T> Scopes<T> {
    fn new(outermost: HashMap<String, T>) -> Self {
        Scopes(vec![outermost])
    }

    fn enter(&mut self) {
        self.0.push(HashMap::new());
    }

    fn exit(&mut self) {
        self.0.pop();
    }

    fn bind(&mut self, name: &str, value: T) {
        self.0.last_mut().unwrap().insert(name.to_string(), value);
    }

    /// The innermost binding of `name`, or `None` if nothing binds it.
    fn get(&self, name: &str) -> Option<&T> {
        self.0.iter().rev().find_map(|frame| frame.get(name))
    }
}

struct Analysis<'a> {
    droppable: HashMap<usize, DropKind>,
    /// Scope stack of live candidate owners: name -> declaring `let` identity.
    live: Vec<HashMap<String, usize>>,
    region_depth: usize,
    /// Callables currently believed to hand the caller a value it owns — the
    /// program's functions under the running fixpoint, plus the seeded built-in
    /// producers.
    owned: &'a HashSet<String>,
    /// Whether the function under analysis returns a heap value.
    ret_is_heap: bool,
    /// Whether every heap return seen so far transfers a fresh owned value.
    all_returns_owned: bool,
    /// Declared return type of every callable, for typing a call's result.
    ret_types: &'a HashMap<String, Type>,
    /// The `Owned` table — the only thing that decides how a type is released.
    proto: &'a Owned,
    /// Scope stack of every binding in scope with its type where one is known.
    /// Kept in lock-step with `live`. It under-approximates: a binding whose type
    /// this cannot name is left to leak, never freed as something it is not.
    var_types: Scopes<Option<Type>>,
}

impl Analysis<'_> {
    fn block(&mut self, b: &Block) {
        self.live.push(HashMap::new());
        self.var_types.enter();
        for s in &b.stmts {
            self.stmt(s);
        }
        self.var_types.exit();
        self.live.pop();
    }

    /// Run `body` with `binders` in scope ahead of it — a `for` variable, an
    /// `if let` arm's payload, a lambda's parameters. None of them carries a type
    /// this pass can name, so each is recorded as unknown. Recording is the point:
    /// an unrecorded binder falls through to whatever the enclosing scope calls
    /// that name, which is how an integer loop variable inherited a String.
    fn scoped_block(&mut self, binders: &[String], body: &Block) {
        self.var_types.enter();
        for n in binders {
            self.var_types.bind(n, None);
        }
        self.block(body);
        self.var_types.exit();
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Let {
                name,
                mutable,
                value,
                ty,
                ..
            } => {
                // Account for uses in the initializer *before* the new binding
                // exists (so `let x = x + b` escapes the old `x`).
                self.visit(value);
                // The binding's type: what it was declared, else what the
                // initializer yields. Computed against the *pre-binding* env, so
                // a self-reference resolves to the old value's type. An unknown
                // type is recorded as unknown rather than left out: that is what
                // shadows an outer binding of the same name.
                let bty = ty.clone().or_else(|| self.expr_type(value));
                self.var_types.bind(name, bty.clone());
                // The two questions (RFC-0086 M1). Does the initializer hand over
                // a value nobody else holds? Then the TYPE says how it is
                // released. Neither half is a list of expression forms.
                let owner_kind = if self.transfers(value) {
                    bty.as_ref().and_then(|t| self.proto.release_kind(t))
                } else {
                    None
                };
                if let Some(kind) = owner_kind {
                    // A dynamic string inside a region is owned by the arena, so
                    // skip it. A cell (`ReleaseRef`) lives in the separate slab,
                    // which the region does not touch, so release it regardless.
                    let region_owns = kind == DropKind::FreeStr && self.region_depth > 0;
                    // Arrays are reassigned in place (`a = push(a, x)`), so a
                    // `mut` array can still own a buffer; strings/refs must be
                    // single-assignment to be tracked.
                    // A `mut` Map is mutated in place (`m[k] = v`) and keeps its
                    // identity, so — like an array — it can still own its buffers.
                    let assignable_ok = !*mutable
                        || kind == DropKind::FreeArr
                        || kind == DropKind::FreeSmallArr
                        || kind == DropKind::FreeMap;
                    if assignable_ok && !region_owns {
                        let key = id(s);
                        self.live.last_mut().unwrap().insert(name.clone(), key);
                        self.droppable.insert(key, kind);
                    }
                }
            }
            Stmt::Assign { name, value, .. } => {
                // `a = push(a, ..)` is an in-place self-update: the array keeps
                // its owner. Any *other* reassignment of a tracked binding makes
                // its ownership unclear, so it is dropped from tracking (a safe
                // leak). Pushed values are still accounted for as escapes.
                if self.is_candidate(name) {
                    if let Expr::Call {
                        name: fname, args, ..
                    } = value
                    {
                        let self_update = fname == "push"
                            && matches!(args.first(), Some(Expr::Var { name: a, .. }) if a == name);
                        if self_update {
                            for arg in &args[1..] {
                                self.visit(arg);
                            }
                            return;
                        }
                    }
                    self.escape(name);
                }
                self.visit(value);
            }
            // `name.field = value` stores `value` into a record field; a heap
            // value put there escapes (the record now owns it).
            Stmt::SetField { value, .. } => self.visit(value),
            // `name[i] = value` stores into an array element; a heap value put
            // there escapes (the array now owns it), and an overwritten heap
            // element is not freed (a safe leak — RFC-0011). The array binding
            // itself keeps its owner (the buffer is unchanged), so it is not
            // escaped here. `index` is a scalar; visit it for completeness.
            Stmt::IndexSet { index, value, .. } => {
                self.visit(index);
                self.visit(value);
            }
            Stmt::Return { value, .. } => self.ret(value.as_ref()),
            // `break`/`continue` (RFC-0060) touch no bindings — nothing escapes.
            // Drop emission for the exited scopes is handled by codegen/interp.
            Stmt::Break { .. } | Stmt::Continue { .. } => {}
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                self.visit(cond);
                self.block(then_block);
                if let Some(eb) = else_block {
                    self.block(eb);
                }
            }
            // `if let` (RFC-0060): the scrutinee is visited like any expression;
            // the binders are payload borrows (never auto-freed), so the blocks
            // are walked exactly as an `if`'s are — except that the binders go
            // into scope, so one shadowing an outer String is not read as one.
            Stmt::IfLet {
                pattern,
                scrutinee,
                then_block,
                else_block,
                ..
            } => {
                self.visit(scrutinee);
                self.scoped_block(&pattern_binders(pattern), then_block);
                if let Some(eb) = else_block {
                    self.block(eb);
                }
            }
            Stmt::While { cond, body, .. } => {
                self.visit(cond);
                self.block(body);
            }
            // Iterating escapes the array conservatively: an element may be a
            // pointer into its buffer, so we must not auto-free the array while a
            // bound element could outlive the loop. (Safe leak, never a UAF;
            // an explicit `drop` still reclaims it.)
            Stmt::ForIn {
                var, iter, body, ..
            } => {
                self.visit(iter);
                self.scoped_block(std::slice::from_ref(var), body);
            }
            Stmt::Expr(e) => self.visit(e),
            // `drop name;` reclaims the value explicitly, so it must escape the
            // automatic-drop analysis — otherwise it would be freed twice.
            Stmt::Drop { name, .. } => self.escape(name),
            Stmt::Region { body, .. } => {
                self.region_depth += 1;
                self.block(body);
                self.region_depth -= 1;
            }
        }
    }

    /// Classify a `return`. For a heap-returning function, decide whether the
    /// returned value is a fresh owned allocation being moved out (keeping the
    /// function's owned status) or something borrowed/aliased (which downgrades
    /// it). For a non-heap return, just account for uses.
    fn ret(&mut self, value: Option<&Expr>) {
        let Some(e) = value else { return };
        if !self.ret_is_heap {
            self.visit(e);
            return;
        }
        if self.transfers(e) {
            // `concat(..)`/`cell(..)` read their args (safe); an owned call
            // escapes its args conservatively. Either way the *result* is a
            // fresh owned move. The function's return TYPE already said this is
            // a heap value, so transfer is the whole remaining question.
            self.visit(e);
        } else if let Expr::Var { name, .. } = e {
            if self.is_candidate(name) {
                // Moving a local owner out: it leaves the function, so it must
                // NOT also be dropped here.
                self.escape(name);
            } else {
                // Returning a parameter or an already-escaped value — borrowed.
                self.visit(e);
                self.all_returns_owned = false;
            }
        } else {
            self.visit(e);
            self.all_returns_owned = false;
        }
    }

    /// Whether `e` hands over a value **nobody else holds** — a transfer rather
    /// than an alias.
    ///
    /// This is the half of the decision that is genuinely about the expression,
    /// and it cannot be asked of the type: `at(a, 0)` and `m.keys()` both have an
    /// `Array`'s element or an `Array` for a type, and only one of them allocates.
    /// A literal or an operator result is a value that did not exist a moment ago;
    /// a call answers by its callee (the fixpoint for a declared function, the
    /// seed for a built-in); everything else reads a place somebody else owns.
    ///
    /// Exhaustive on purpose — a new expression form has to answer.
    fn transfers(&self, e: &Expr) -> bool {
        match e {
            Expr::ArrayLit { .. }
            | Expr::MapLit { .. }
            | Expr::StructLit { .. }
            | Expr::Binary { .. }
            | Expr::Unary { .. } => true,
            Expr::Call { name, .. } => self.owned.contains(name.as_str()),
            // A string literal is static storage, not an allocation.
            Expr::Str(_)
            | Expr::Int(_)
            | Expr::Byte(_)
            | Expr::Float(_)
            | Expr::Bool(_)
            // A place read is an alias of what it reads.
            | Expr::Var { .. }
            | Expr::Field { .. }
            // The rest may each yield a fresh value, and none of them is
            // *provably* doing so from the form alone, so each is left to leak —
            // always safe, never a double free.
            | Expr::Try { .. }
            | Expr::Match { .. }
            | Expr::IfExpr { .. }
            | Expr::TryConstruct { .. }
            | Expr::Spawn { .. }
            | Expr::Lambda { .. } => false,
        }
    }

    /// The type of `e`, where this pass can name it.
    ///
    /// It is a *declared-types* pass, not a re-run of the checker: parameters,
    /// `let` annotations, function return types and literal shapes, propagated
    /// through the scope stack. When unsure it answers `None`, which leaves the
    /// binding to leak — always safe, never freed as something it is not.
    fn expr_type(&self, e: &Expr) -> Option<Type> {
        match e {
            Expr::Str(_) => Some(Type::Str),
            Expr::Int(_) | Expr::Byte(_) => Some(Type::Int),
            Expr::Float(_) => Some(Type::Float),
            Expr::Bool(_) => Some(Type::Bool),
            Expr::Var { name, .. } => self.var_types.get(name).cloned().flatten(),
            Expr::Call { name, .. } => self.ret_types.get(name).cloned(),
            // The one allocating operator is `+` on Strings, and its result has
            // its left operand's type. Every other operator is a scalar, whose
            // type answers `None` from the seed anyway.
            Expr::Binary {
                op: BinOp::Add,
                lhs,
                ..
            } => self.expr_type(lhs),
            // An array literal does NOT name its own type, and that is the whole
            // reason it is absent here: `[1, 2, 3]` is an `ArrayN` held inline,
            // `[]` annotated `Array<T>` is three words around a buffer, and
            // annotated `SmallArray<T, N>` is a header whose buffer is null until
            // it spills. Three layouts, one syntax. Answering `Array` for all of
            // them freed a fixed array's stack storage and corrupted the heap, so
            // the annotation is the only thing that may answer.
            //
            // A map literal has no such second shape: `[:]` is a `Map` and there
            // is nothing else it could be.
            Expr::MapLit { .. } => Some(Type::Map(Box::new(Type::Str), Box::new(Type::Unit))),
            Expr::StructLit { name, .. } => Some(Type::Named(name.clone())),
            _ => None,
        }
    }

    /// Walk an expression, escaping any candidate used outside a safe read.
    fn visit(&mut self, e: &Expr) {
        match e {
            Expr::Int(_) | Expr::Byte(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => {}
            Expr::Var { name, .. } => self.escape(name),
            Expr::Unary { expr, .. } | Expr::Try { expr, .. } => self.visit(expr),
            // `x.length` / `s.byteLength` read the length header only — a safe read
            // of a candidate (RFC-0058 renamed a String's byte length). Any other
            // field access is a conservative escape.
            Expr::Field { expr, field, .. } if field == "length" || field == "byteLength" => {
                self.operand(expr)
            }
            Expr::Field { expr, .. } => self.visit(expr),
            Expr::Binary { lhs, rhs, .. } => {
                // `==`/`!=` and string `+` only *read* their operands (concat
                // copies both into a fresh buffer, never retaining them), so a
                // whole candidate on either side is a safe read. Other operators
                // are numeric, whose operands are never tracked candidates — so
                // treating them as reads too is harmless and simpler.
                self.operand(lhs);
                self.operand(rhs);
            }
            Expr::Call { name, args, .. } => {
                // These builtins only *read* their heap argument and never retain
                // it — a whole candidate passed to one is a safe use: `print` /
                // `@concat` for strings, `get` for references,
                // and the log methods (which format-and-write their message).
                // `release` is intentionally excluded: it hands the cell off, so
                // it escapes the binding (no auto-release on top of it). `logger`
                // is excluded too: it *returns* its name argument (an alias). Any
                // other call may alias its argument into its result (e.g.
                // `fn id(s) { return s; }`), so it counts as an escape too.
                //
                // `set(c, v)` reads its *Ref* argument but STORES `v` in the cell
                // — the cell outlives the block, so `v` must escape (a droppable
                // `v` would be freed at block exit while the cell still points at
                // it: a use-after-free on the next `get`).
                if name == "set" {
                    if let Some((c, rest)) = args.split_first() {
                        self.operand(c);
                        for a in rest {
                            self.visit(a);
                        }
                    }
                } else if matches!(
                    name.as_str(),
                    "print" | "@concat" | "get" | "at" | "alen"
                        // `@pop`/`@swapRemove` mutate the array in place but do
                        // not free its buffer, so the receiver stays a live owner
                        // (a safe read); the removed element is a safe leak.
                        | "@pop" | "@swapRemove"
                        // Map methods (RFC-0028) mutate/read in place but never
                        // free the map's buffers, so the receiver stays a live
                        // owner (a safe read); `@keys` returns a fresh snapshot.
                        | "@has" | "@remove" | "@keys"
                        | "trace" | "debug" | "info" | "warn" | "error"
                ) {
                    for a in args {
                        self.operand(a);
                    }
                } else {
                    for a in args {
                        self.visit(a);
                    }
                }
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.visit(scrutinee);
                for arm in arms {
                    // The arm's payload binders shadow the enclosing scope for
                    // the arm body, exactly as a `let` would.
                    self.var_types.enter();
                    for n in pattern_binders(&arm.pattern) {
                        self.var_types.bind(&n, None);
                    }
                    self.visit(&arm.body);
                    self.var_types.exit();
                }
            }
            Expr::IfExpr {
                cond,
                then_branch,
                else_branch,
                ..
            } => {
                self.visit(cond);
                self.visit(then_branch);
                if let Some(eb) = else_branch {
                    self.visit(eb);
                }
            }
            Expr::StructLit { fields, .. } => {
                for (_, v) in fields {
                    self.visit(v);
                }
            }
            Expr::TryConstruct { args, .. } => {
                for a in args {
                    self.visit(a);
                }
            }
            Expr::ArrayLit { elems, .. } => {
                for e in elems {
                    self.visit(e);
                }
            }
            Expr::MapLit { entries, .. } => {
                for (k, v) in entries {
                    self.visit(k);
                    self.visit(v);
                }
            }
            Expr::Spawn { args, .. } => {
                for e in args {
                    self.visit(e);
                }
            }
            // A lambda body (RFC-0023): a captured heap binding is passed by value
            // into the monomorphized lambda function, which never frees it (the
            // enclosing scope keeps ownership). Walking the body conservatively
            // treats a captured candidate as escaped, so it is not auto-freed at
            // the capture site — sound (never a double-free; at worst a leak, which
            // does not affect observable behavior or parity).
            //
            // The parameters are untyped here, so none of them is provably a
            // String and each is recorded as not one — which costs nothing a
            // parameter did not already lack, and stops one named like an
            // enclosing String from being read as that String.
            Expr::Lambda { params, body, .. } => {
                self.var_types.enter();
                for p in params {
                    self.var_types.bind(p, None);
                }
                match body {
                    LambdaBody::Expr(e2) => self.visit(e2),
                    LambdaBody::Block(b) => {
                        for s in &b.stmts {
                            self.stmt(s);
                        }
                    }
                }
                self.var_types.exit();
            }
        }
    }

    /// A position where a whole candidate variable is only *read*, not retained.
    fn operand(&mut self, e: &Expr) {
        match e {
            Expr::Var { name, .. } if self.is_candidate(name) => { /* safe read */ }
            _ => self.visit(e),
        }
    }

    fn is_candidate(&self, name: &str) -> bool {
        self.live.iter().rev().any(|f| f.contains_key(name))
    }

    /// Mark the innermost candidate named `name`, if any, as escaped: no longer
    /// droppable and no longer tracked.
    fn escape(&mut self, name: &str) {
        for frame in self.live.iter_mut().rev() {
            if let Some(key) = frame.remove(name) {
                self.droppable.remove(&key);
                return;
            }
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
mod tests {
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

    #[test]
    fn does_not_free_aliased_temporary() {
        let src = "fn main() -> Int64 { let a = \"x\"; let b = \"y\"; \
                   let s = a + b; let t = s; return t.length; }";
        assert_eq!(drop_count(src, "main"), 0);
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

    #[test]
    fn skips_mutable_binding() {
        let src = "fn main() -> Int64 { let a = \"x\"; let b = \"y\"; \
                   let mut s = a + b; return s.length; }";
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
        assert_eq!(drop_count(src, "main"), 0);
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

    #[test]
    fn identity_returning_param_is_not_owned() {
        let src = "fn id(s: String) -> String { return s; } fn main() -> Int64 { return 0; }";
        let (o, _) = analyze_src(src);
        assert!(!o.owned_fns.contains_key("id"));
    }

    #[test]
    fn mixed_return_paths_are_not_owned() {
        let src = "fn pick(c: Bool, a: String, b: String) -> String { \
                       if c { return a + b; } return a; } \
                   fn main() -> Int64 { return 0; }";
        let (o, _) = analyze_src(src);
        assert!(!o.owned_fns.contains_key("pick"));
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

    #[test]
    fn caller_does_not_free_borrowed_call_result() {
        // `id` is not owned, so its result must not be freed by the caller.
        let src = "fn id(s: String) -> String { return s; } \
                   fn main() -> Int64 { let a = \"x\"; let b = \"y\"; \
                       let s = a + b; let y = id(s); return y.length; }";
        let (o, _) = analyze_src(src);
        // `s` escapes into the `id(..)` call, `y` is not an owned result:
        assert_eq!(o.droppable.get("main").map(|s| s.len()).unwrap_or(0), 0);
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

    #[test]
    fn a_record_that_declares_nothing_is_reclaimed_by_nothing() {
        // The mirror. A record is a value the engines copy; without a declared row
        // there is nothing to call, and inventing one would free a field somebody
        // else still holds.
        let src = "type Ring = { slots: Array<Int64> } \
                   fn make() -> Ring { return Ring { slots: [] } } \
                   fn main() -> Int64 { let r = make(); return 0; }";
        let (o, _) = analyze_src(src);
        assert!(!o.owned_fns.contains_key("make"));
        assert_eq!(drop_count(src, "main"), 0);
    }

    #[test]
    fn factory_returning_cell_is_owned() {
        let src =
            "fn make(v: Int64) -> Ref<Int64> { return cell(v); } fn main() -> Int64 { return 0; }";
        let (o, _) = analyze_src(src);
        assert_eq!(o.owned_fns.get("make"), Some(&DropKind::ReleaseRef));
    }
}
