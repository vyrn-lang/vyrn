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
    /// Protocol method name -> protocol name, for the receiver-directed
    /// fallback in `type_of`'s Call arm: a method whose DECLARED return
    /// mentions a type variable (an associated type) answers per impl, and
    /// the flattened impl row spells it concretely.
    method_protos: HashMap<String, String>,
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
        let mut method_protos: HashMap<String, String> = HashMap::new();
        for p in &program.protocols {
            for m in &p.methods {
                if m.result_cap.is_none() && !crate::types::mentions_param(&m.ret) {
                    rets.entry(m.name.clone()).or_insert_with(|| m.ret.clone());
                }
                if m.result_cap.is_none() {
                    method_protos.insert(m.name.clone(), p.name.clone());
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
            owned: crate::own::Owned::new(program),
            method_protos,
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

    /// See [`crate::own::Owned::reaches_declared`].
    pub fn reaches_declared(&self, ty: &Type) -> bool {
        self.owned.reaches_declared(ty)
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
            Expr::Call { name, args, .. } => self
                .rets
                .get(name)
                .cloned()
                // A GENERIC function's declared return mentions its type
                // variables, and a binding to it typed as unknown — `let d =
                // twice(s)` where `twice<T>` returns `Array<T>` had no
                // release row and leaked the array with everything in it
                // (exit-residue round thirty-seven). The variables are solved
                // from the arguments' own types, exactly as the checker
                // solves them at the call; a variable no argument names stays
                // unsolved, and the callers' resolve guards stand down as
                // before.
                .map(|t| {
                    if !crate::types::mentions_param(&t) {
                        return t;
                    }
                    let Some(ps) = self.params.get(name) else {
                        return t;
                    };
                    let mut subst = HashMap::new();
                    for (p, a) in ps.iter().zip(args) {
                        // A bare top-level FUNCTION name types as its own fn
                        // row FOR SOLVING ONLY (round fifty-seven): without
                        // it `defer(greet)` never solved `Deferred<P, T>` and
                        // `force(..)`'s printed result had no release row.
                        // Kept out of the general `Var` arm on purpose — a
                        // fn name is static, and typing it everywhere made
                        // `Deferred { run: label }` a MOVE of `label`.
                        let at = self.type_of(vars, a).or_else(|| match a {
                            Expr::Var { name: f, .. } if vars.get(f).is_none() => {
                                let fps = self.params.get(f)?;
                                let ret = self.rets.get(f)?;
                                Some(Type::Fn(fps.clone(), Box::new(ret.clone())))
                            }
                            _ => None,
                        });
                        if let Some(at) = at {
                            crate::types::solve_param(p, &at, &mut subst);
                        }
                    }
                    crate::types::substitute(&t, &subst)
                })
                // A call through a `fn`-typed BINDING (`df(13)` where `let df
                // = d.run`): the binding's own type names the return. Sound
                // because a `fn` value's result is always OWNED — a lambda
                // returning a captured heap value raw is refused by movecheck
                // (rule 3 reaches closures; exit-residue round nine's pin
                // showed the emitted body was `ret ptr %cap`, no copy), and a
                // function's return was already owned by rule 3. Round six
                // refused this arm for want of exactly that guarantee.
                .or_else(|| {
                    match vars
                        .get(name)
                        .cloned()
                        .flatten()
                        .map(|t| crate::types::resolve(&t, &self.decls))
                    {
                        Some(Type::Fn(_, ret)) => Some(*ret),
                        _ => None,
                    }
                })
                // `Some(x)` is `Option<type_of(x)>` (round fifty-seven): the
                // bare owner name the variants table answers is a generic
                // with no instantiation, so `let root = Some(insert(..))`
                // carried no release row and its payload box leaked. `Ok`/
                // `Err` stay with the table — one arm cannot name the other
                // side's type.
                .or_else(|| {
                    if name != "Some" {
                        return None;
                    }
                    args.first()
                        .and_then(|a| self.type_of(vars, a))
                        .map(|t| Type::Option(Box::new(t)))
                })
                .or_else(|| {
                    self.variants
                        .get(name)
                        .and_then(|owner| owner.clone())
                        .map(Type::Named)
                })
                // A protocol method whose declared return mentions a type
                // variable — an associated type — was never seeded under its
                // surface name, so the chain above answers nothing
                // (exit-residue round forty-three: `s.valueOr("none")` typed
                // as unknown and its result carried no row). The receiver's
                // type picks the impl, exactly as the checker dispatches, and
                // the flattened impl row already spells the return
                // concretely; a generic impl's row still mentions its
                // variables and stands down.
                .or_else(|| {
                    let proto = self.method_protos.get(name)?;
                    let rt = self.type_of(vars, args.first()?)?;
                    let k = crate::types::type_key(&rt)?;
                    let m = crate::types::impl_method_name(proto, &k, name);
                    self.rets
                        .get(&m)
                        .cloned()
                        .filter(|r| !crate::types::mentions_param(r))
                }),
            // A FIELD READ answers the record's declaration — `let df =
            // d.run` on a fn-typed field typed as unknown, and everything
            // downstream of the binding followed (exit-residue round
            // forty-six). `forced` unwraps a `lazy T` to the value a read
            // yields. A generic record's field may still mention its
            // variables; the caller's resolve guards stand down as usual.
            Expr::Field { expr, field, .. } => {
                let bt = self.type_of(vars, expr)?;
                match crate::types::resolve(&bt, &self.decls) {
                    Type::Record(fields) => fields
                        .iter()
                        .find(|f| &f.name == field)
                        .map(|f| crate::types::forced(&f.ty)),
                    _ => None,
                }
            }
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
            Expr::Try { expr, .. } => self.success_payload(vars, expr),
            // `a ?? b` is the `match` the parser spells for it
            // (`Parser::nullish`): a `Success(@v) => @v` arm and a `Failure`
            // arm. Its value is the scrutinee's success payload, exactly as
            // `e?`'s is, so the one rule answers both. Without this a
            // `let t = s ?? "y"` typed as unknown, and `vyrn why --memory`
            // called a String "no heap" (RFC-0126 §8.8).
            Expr::Match {
                scrutinee, arms, ..
            } if arms.len() == 2
                && matches!(&arms[1].pattern, Pattern::Failure(_))
                && matches!(
                    (&arms[0].pattern, &arms[0].body),
                    (Pattern::Success(v), ArmBody::Expr(Expr::Var { name, .. })) if v == name
                ) =>
            {
                self.success_payload(vars, scrutinee)
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

    /// The success payload of a `?` or `??` operand. An `Option<T>` or a
    /// `Result<T, _>` yields `T`. A Fallible operand (RFC-0080 M3) yields
    /// what ITS `success` returns — the parser substituted the associated
    /// `Output` into the flattened impl method, so the mangled row already
    /// spells it concretely. Without this a `let body = fetch(code)?` typed
    /// as unknown and the copied-out payload carried no release row
    /// (exit-residue round forty-one).
    fn success_payload(&self, vars: &Scopes<Option<Type>>, operand: &Expr) -> Option<Type> {
        let ot = self.type_of(vars, operand)?;
        let r = crate::types::resolve(&ot, &self.decls);
        match crate::types::option_payload(&r)
            .or_else(|| crate::types::result_payloads(&r).map(|(t, _)| t))
        {
            Some(t) => Some(t.clone()),
            None => crate::types::type_key(&ot).and_then(|k| {
                self.rets
                    .get(&crate::types::impl_method_name(
                        crate::types::FALLIBLE,
                        &k,
                        "success",
                    ))
                    .cloned()
            }),
        }
    }
}
