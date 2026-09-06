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

    /// See [`crate::own::Owned::reaches_declared`].
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
