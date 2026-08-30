//! The **declared-types** reading of a program (RFC-0089 M2, Phase 4a).
//!
//! Two passes need to know the type of an expression without being the type
//! checker. [`own`](crate::own) needs it to decide how a binding is released.
//! [`movecheck`](crate::movecheck) needs it to decide whether a value moves.
//! Before this module `own.rs` held the only answer, as a private method on its
//! own walker, so the second pass had no way to ask.
//!
//! This is the same reading, lifted out and shared — **not a third walker**.
//! RFC-0087 records three times what happens when two walkers over `Expr` can
//! disagree, so there is one [`Declared::type_of`] and both passes call it. The
//! passes keep their own traversals, because they visit for different reasons;
//! what they share is the answer.
//!
//! It reads only what the program **declares**: a parameter's type, a `let`
//! annotation, a function's return type, a literal's shape, propagated through
//! the scope stack. When it cannot name a type it says `None`.
//!
//! **The two directions of "unknown" are opposite, and a caller must pick one.**
//! `own.rs` treats `None` as "do not release", so an unknown type leaks, which
//! is always safe. A move check may not do the same: a skipped move is a
//! use-after-free, not a leak. Phase 4b decides what `None` means there; 4a only
//! reports how often it happens.

use std::collections::HashMap;

use crate::ast::*;

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

    /// The index of the frame that binds `name`, innermost first.
    ///
    /// A lambda capture is exactly "resolves to a frame below the lambda's own",
    /// so the frame index is what tells a capture from a local.
    pub fn frame_of(&self, name: &str) -> Option<usize> {
        self.0.iter().rposition(|frame| frame.contains_key(name))
    }

    /// How many frames are on the stack.
    pub fn depth(&self) -> usize {
        self.0.len()
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
    owned: crate::own::Owned,
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
        let decls = crate::types::decl_map(program);
        let mut variants: HashMap<String, Option<String>> =
            ["Some", "Ok", "Err", "Success", "Failure"]
                .into_iter()
                .map(|n| (n.to_string(), None))
                .collect();
        for d in decls.values() {
            if let Type::Enum(vs) = &d.base {
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
            owned: crate::own::Owned::new(program),
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

    /// The type declarations, for a caller that asks `crate::types` directly.
    pub fn decls(&self) -> &HashMap<String, TypeDecl> {
        &self.decls
    }

    /// Module state and its declared type, as a scope frame.
    pub fn globals(&self) -> HashMap<String, Option<Type>> {
        self.globals.clone()
    }

    /// Whether a value of `ty` transitively owns heap — RFC-0089 rule 1, through
    /// the one implementation in [`crate::own::owns_heap`].
    pub fn owns_heap(&self, ty: &Type) -> bool {
        crate::own::owns_heap(ty, &self.decls)
    }

    /// Whether `ty` carries a must-use obligation, and which row says so — the
    /// opt-in linear rows (RFC-0086 M3), through the one implementation in
    /// [`crate::own::Owned::linear_kind`]. A seeded `Stream` and an
    /// `impl MustUse for T` are read out of the same table, so nothing in the
    /// compiler asks this question twice.
    pub fn linear_kind(&self, ty: &Type) -> Option<crate::own::Linear> {
        self.owned.linear_kind(ty)
    }

    /// [`Declared::linear_kind`] with the row forgotten.
    pub fn must_use(&self, ty: &Type) -> bool {
        self.owned.must_use(ty)
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

    pub fn type_of(&self, vars: &Scopes<Option<Type>>, e: &Expr) -> Option<Type> {
        match e {
            Expr::Str(_) => Some(Type::Str),
            Expr::Int(_) | Expr::Byte(_) => Some(Type::Int),
            Expr::Float(_) => Some(Type::Float),
            Expr::Bool(_) => Some(Type::Bool),
            Expr::Var { name, .. } => vars.get(name).cloned().flatten(),
            // `x.copy()` returns its receiver's type — the one builtin whose
            // result type is not a fixed row.
            Expr::Call { name, args, .. } if name == "@copy" => {
                args.first().and_then(|a| self.type_of(vars, a))
            }
            // `m.keys()` answers `Array<K>` where `K` is the RECEIVER's key
            // type (RFC-0117). The seeded row spells `Array<K>` and this
            // reading never solves a parameter, so an unsolved `K` here made
            // the snapshot's release buffer-only — a `Map<String, V>`'s key
            // snapshot leaked every String it held. `own`'s
            // `a_fresh_key_snapshot_is_released` is the row that caught it.
            Expr::Call { name, args, .. } if name == "@keys" => {
                match crate::types::resolve(&self.type_of(vars, args.first()?)?, &self.decls) {
                    Type::Map(k, _) => Some(Type::Array(k)),
                    _ => None,
                }
            }
            // `m.tally(..)` / `m.tallyBytes(..)` answer the receiver's own map
            // type — solved the same way, for the same reason.
            Expr::Call { name, args, .. } if name == "@tally" || name == "@tallyBytes" => {
                args.first().and_then(|a| self.type_of(vars, a))
            }
            // `fromJson(T, s)` answers `Validation<T>` — the call is
            // type-directed, so `rets` has no row for it, and the binding it
            // usually lands in typed as unknown: the decoded tree's owner had
            // no release row (exit-residue round four, the decode half).
            Expr::Call { name, args, .. } if name == "fromJson" => match args.first() {
                Some(Expr::Var { name: tn, .. }) => Some(Type::App(
                    "Validation".to_string(),
                    vec![Type::Named(tn.clone())],
                )),
                _ => None,
            },
            // A variant constructor answers the enum it constructs — `JObj(..)`
            // IS a `Json`, which is what lets an unannotated `let doc =
            // JObj(..)` carry a release row at all. Without this arm the
            // binding typed as unknown and the whole tree leaked at block
            // exit, `impl Owned for Json` notwithstanding (exit-residue round
            // four: jchain's 24 blocks, and the json/html/graphql family
            // behind it). The guard against a shared or generic variant name
            // is in the table's construction.
            Expr::Call { name, .. } => self
                .rets
                .get(name)
                .cloned()
                // A call through a `fn`-typed BINDING (`df(13)` where `let df
                // = d.run`) is deliberately NOT answered, though the binding's
                // type names the return. Answering it records the result as a
                // caller-owned temporary — and a lambda may return a CAPTURE
                // or a parameter, which no reading of the CALL can see, so
                // the free would be a use-after-free the moment such a value
                // flows through. `fnvalarg`'s seven small blocks are the
                // recorded price of fn-value opacity; an arm here needs a pin
                // proving the capture-returning shape copies on return first
                // (exit-residue round six).
                .or_else(|| {
                    self.variants
                        .get(name)
                        .and_then(|owner| owner.clone())
                        .map(Type::Named)
                }),
            // The one allocating operator is `+` on Strings, and its result has
            // its left operand's type. Every other operator is a scalar, whose
            // type owns no heap anyway.
            Expr::Binary {
                op: BinOp::Add,
                lhs,
                ..
            } => self.type_of(vars, lhs),
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
            // ---- the forms whose result is a value, not a place (census §2a) --
            // Each of these yields one of its arms, and the checker has already
            // made the arms agree, so the first one that names a type answers for
            // all of them. Until Phase 4c they were absent, which is why an
            // if-expression that built a String had no owner.
            // An if-expression binds nothing, so its arms read in this scope.
            // A `match` arm does bind, and the binders have to be in scope before
            // its body can be read — `match o { Some(m) => m + 1 }` under an
            // outer String `m` reads as a concatenation otherwise, and the
            // backend then frees the integer 2. `movecheck` answers that one,
            // because it is the pass that can bind a payload.
            Expr::IfExpr {
                then_branch,
                else_branch,
                ..
            } => self
                .type_of(vars, then_branch)
                .or_else(|| else_branch.as_ref().and_then(|e| self.type_of(vars, e))),
            // `e?` yields the success payload of what `e` is.
            Expr::Try { expr, .. } => {
                match crate::types::resolve(&self.type_of(vars, expr)?, &self.decls) {
                    Type::Option(t) | Type::Result(t, _) => Some(*t),
                    _ => None,
                }
            }
            // A fallible construction (RFC-0009) answers an OPTION of its own
            // type — `Age?(n)` is `Option<Age>`, which is what every other
            // reading of it says (the direct backend's `Expr::TryConstruct` arm,
            // and the checker's). Reading it as the bare `Age` cost nothing
            // while the corpus only refined numbers: `own` released the slot at
            // the payload's width, and an Int payload owns no heap so the
            // release was a no-op. RFC-0098 put a String-based option type in an
            // `if let` scrutinee, and the release then read the sum's TAG word as
            // a String pointer and freed it — an access violation on the first
            // line of the program.
            Expr::TryConstruct { name, .. } => {
                Some(Type::Option(Box::new(Type::Named(name.clone()))))
            }
            Expr::Spawn { name, .. } => self
                .rets
                .get(name)
                .cloned()
                .map(|t| Type::Task(Box::new(t))),
            _ => None,
        }
    }
}
