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

/// The built-in calls that hand the caller a fresh heap value, with the type of
/// that value. **This is a fact about a function, not about a type** — `at(a, 0)`
/// and `m.keys()` both return an element of a container and only one of them
/// allocates — so it cannot be derived from a signature and the compiler knows it
/// intrinsically, exactly as it knows the seeded `Owned` rows.
///
/// It under-approximates on purpose. A builtin missing from here leaks, which is
/// always safe; one wrongly present frees memory somebody still holds.
pub fn builtin_producers() -> impl Iterator<Item = (&'static str, Type)> {
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

/// The program-level tables the declared-types reading needs: what each named
/// type is, what each callable returns, and what each module-state binding holds.
///
/// Built once per program. A per-body [`Scopes`] carries the rest.
pub struct Declared {
    /// Every `type X = ..`, so a nominal type answers as its base does.
    decls: HashMap<String, TypeDecl>,
    /// The declared return type of every callable this can name: the program's
    /// functions plus the built-in producers. A user declaration wins, though
    /// `checker::RESERVED` already forbids taking one of these names.
    rets: HashMap<String, Type>,
    /// Module state (RFC-0013), with its declared type where it has one. Seeded
    /// into the outermost scope frame by a pass that wants globals typed.
    globals: HashMap<String, Option<Type>>,
}

impl Declared {
    pub fn new(program: &Program) -> Self {
        let mut rets: HashMap<String, Type> = HashMap::new();
        for (n, t) in builtin_producers() {
            rets.insert(n.to_string(), t);
        }
        for f in &program.functions {
            rets.insert(f.name.clone(), f.ret.clone());
        }
        Declared {
            decls: crate::types::decl_map(program),
            rets,
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
            Expr::Call { name, .. } => self.rets.get(name).cloned(),
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
            _ => None,
        }
    }
}
