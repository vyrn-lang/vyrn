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
//! A fresh heap value is produced by `a + b` on Strings (the `@concat`/`@str`
//! internal spellings), or by a call to a function that itself returns owned
//! (computed by fixpoint over the call graph). A binding is droppable unless it
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
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DropKind {
    /// A dynamic `String` — `free` the buffer (Path A).
    FreeStr,
    /// A generational reference — `release` the cell (Path B).
    ReleaseRef,
    /// A growable array — `afree` the backing buffer.
    AfreeArr,
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
}

/// Analyse ownership across a whole program.
pub fn analyze(program: &Program) -> Ownership {
    // Named types over `String`, and functions returning a String-like type —
    // the light context the `a + b` string classifier needs (see `str_vars`).
    let string_types: HashSet<String> = program
        .type_decls
        .iter()
        .filter(|d| matches!(d.base, Type::Str))
        .map(|d| d.name.clone())
        .collect();
    let string_fns: HashSet<String> = program
        .functions
        .iter()
        .filter(|f| is_string_like(&f.ret, &string_types))
        .map(|f| f.name.clone())
        .collect();

    // Seed optimistically: every heap-returning function might return owned.
    let mut owned: HashMap<String, DropKind> = program
        .functions
        .iter()
        .filter_map(|f| returns_owned_kind(&f.ret).map(|k| (f.name.clone(), k)))
        .collect();

    // Fixpoint: remove any function that has a non-owned heap return under the
    // current assumptions. Monotone (only shrinks), so it terminates.
    loop {
        let mut changed = false;
        let snapshot = owned.clone();
        for f in &program.functions {
            if snapshot.contains_key(&f.name)
                && !analyze_fn(f, &snapshot, &string_fns, &string_types).is_owned
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
        let d = analyze_fn(f, &owned, &string_fns, &string_types).droppable;
        fresh_refs.extend(fresh_refs_in(&f.body, &d));
        droppable.insert(f.name.clone(), d);
    }
    // Test bodies (RFC-0015) get the same block-exit drop analysis so a `let` in
    // a test reclaims its heap value exactly as it would in a function. The body
    // is the REAL node the interpreter walks, so the by-address droppable keys
    // match at run time. Tests never return an owned value (they are `Unit`).
    for (i, t) in program.tests.iter().enumerate() {
        let d = analyze_body(
            &[],
            &t.body,
            &Type::Unit,
            &owned,
            &string_fns,
            &string_types,
        )
        .droppable;
        fresh_refs.extend(fresh_refs_in(&t.body, &d));
        droppable.insert(format!("test@{i}"), d);
    }
    // Bench bodies (RFC-0055) get the same block-exit drop analysis, keyed by the
    // synthetic `bench@<index>` name the interpreter (`--check`) walks.
    for (i, b) in program.benches.iter().enumerate() {
        let d = analyze_body(
            &[],
            &b.body,
            &Type::Unit,
            &owned,
            &string_fns,
            &string_types,
        )
        .droppable;
        fresh_refs.extend(fresh_refs_in(&b.body, &d));
        droppable.insert(format!("bench@{i}"), d);
    }
    Ownership {
        owned_fns: owned,
        droppable,
        fresh_refs,
    }
}

/// Whether `ty` is a `String` or a nominal type whose base is `String`.
fn is_string_like(ty: &Type, string_types: &HashSet<String>) -> bool {
    match ty {
        Type::Str => true,
        Type::Named(n) => string_types.contains(n),
        _ => false,
    }
}

/// The reclamation kind a function transfers to its caller, if its return type is
/// a heap value the caller then owns. `String` → free; `Ref` → release. Nominal
/// string types and records-with-strings are left out for now (they leak — safe).
fn returns_owned_kind(ty: &Type) -> Option<DropKind> {
    match ty {
        Type::Str => Some(DropKind::FreeStr),
        Type::Ref(_) => Some(DropKind::ReleaseRef),
        Type::Array(_) => Some(DropKind::AfreeArr),
        Type::SmallArray(..) => Some(DropKind::FreeSmallArr),
        Type::Map(..) => Some(DropKind::FreeMap),
        _ => None,
    }
}

struct FnResult {
    droppable: HashMap<usize, DropKind>,
    is_owned: bool,
}

fn analyze_fn(
    f: &Function,
    owned: &HashMap<String, DropKind>,
    string_fns: &HashSet<String>,
    string_types: &HashSet<String>,
) -> FnResult {
    analyze_body(&f.params, &f.body, &f.ret, owned, string_fns, string_types)
}

/// The core of [`analyze_fn`], parameterized over a body directly so a test body
/// (RFC-0015) — which has no surrounding `Function` node — can be analysed with
/// the SAME node addresses the interpreter walks (a clone would not match).
fn analyze_body(
    params_list: &[Param],
    body: &Block,
    ret: &Type,
    owned: &HashMap<String, DropKind>,
    string_fns: &HashSet<String>,
    string_types: &HashSet<String>,
) -> FnResult {
    // Seed the outermost string-var scope with every parameter, each carrying
    // whether it is a String — a non-String parameter has to be recorded too, or
    // a `let` shadowing it further in would resolve to nothing and fall through.
    let params: HashMap<String, bool> = params_list
        .iter()
        .map(|p| (p.name.clone(), is_string_like(&p.ty, string_types)))
        .collect();
    let mut a = Analysis {
        droppable: HashMap::new(),
        live: vec![HashMap::new()],
        region_depth: 0,
        owned,
        ret_is_heap: returns_owned_kind(ret).is_some(),
        all_returns_owned: true,
        string_fns,
        str_vars: Scopes::new(params),
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
    /// Functions currently believed to return owned values, with their kind.
    owned: &'a HashMap<String, DropKind>,
    /// Whether the function under analysis returns a heap value.
    ret_is_heap: bool,
    /// Whether every heap return seen so far transfers a fresh owned value.
    all_returns_owned: bool,
    /// Names of functions whose return type is a `String` (or a nominal type
    /// over `String`). Used to classify `a + b` as string concatenation when an
    /// operand is a call — a fresh heap String the caller then owns.
    string_fns: &'a HashSet<String>,
    /// Scope stack of every binding in scope, each saying whether it is a
    /// `String`. Kept in lock-step with `live`; lets `a + b` be recognised as a
    /// string concat (not integer arithmetic) without a full re-typing pass. It
    /// only ever under-approximates — an unrecognised string temporary is left to
    /// leak, never freed as if it were an integer.
    str_vars: Scopes<bool>,
}

impl Analysis<'_> {
    fn block(&mut self, b: &Block) {
        self.live.push(HashMap::new());
        self.str_vars.enter();
        for s in &b.stmts {
            self.stmt(s);
        }
        self.str_vars.exit();
        self.live.pop();
    }

    /// Run `body` with `binders` in scope ahead of it — a `for` variable, an
    /// `if let` arm's payload, a lambda's parameters. None of them can be proved
    /// a String, so each is recorded as not one. Recording is the point: an
    /// unrecorded binder falls through to whatever the enclosing scope calls that
    /// name, which is how an integer loop variable inherits a String.
    fn scoped_block(&mut self, binders: &[String], body: &Block) {
        self.str_vars.enter();
        for n in binders {
            self.str_vars.bind(n, false);
        }
        self.block(body);
        self.str_vars.exit();
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
                // Record whether the binding is a `String`, so later `a + b` on
                // it is seen as concatenation. Computed against the *pre-binding*
                // env, so a self-reference resolves to the old value's type. A
                // non-String is recorded as such rather than left out: that is
                // what shadows an outer String of the same name.
                let is_str = self.expr_is_string(value);
                self.str_vars.bind(name, is_str);
                // A `SmallArray<T, N>` binding (RFC-0056) owns its `data` buffer,
                // which is null while inline (so `free` is a no-op) and heap once
                // spilled. Unlike a growable `Array`, its only producers are the
                // `[]`/`[..]` literals (there is no `array()`-style call), so
                // track it whenever the slot is a `SmallArray` — reclaim at scope
                // end via `FreeSmallArr` (escape analysis un-tracks a returned or
                // aliased one, exactly as for `Array`, so it is freed once).
                let produced = self.owner_producing(value);
                let owner_kind = if matches!(ty, Some(Type::SmallArray(..))) {
                    Some(DropKind::FreeSmallArr)
                } else {
                    produced
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
                        || kind == DropKind::AfreeArr
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
            // explicit `afree` still reclaims it.)
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
        if self.owner_producing(e).is_some() {
            // `concat(..)`/`cell(..)` read their args (safe); an owned call
            // escapes its args conservatively. Either way the *result* is a
            // fresh owned move.
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

    /// The reclamation kind if `e` yields a fresh heap value the binding owns:
    /// `@concat`/`@str` or `a + b` on Strings → a string, `cell` → a reference,
    /// or a call to an owned function (its declared kind). Otherwise `None`.
    fn owner_producing(&self, e: &Expr) -> Option<DropKind> {
        match e {
            Expr::Call { name, .. } if name == "@concat" || name == "@str" => {
                Some(DropKind::FreeStr)
            }
            // `a + b` on Strings allocates a fresh String, exactly like `@concat`.
            Expr::Binary { op: BinOp::Add, .. } if self.expr_is_string(e) => {
                Some(DropKind::FreeStr)
            }
            Expr::Call { name, .. } if name == "cell" => Some(DropKind::ReleaseRef),
            Expr::Call { name, .. } if name == "array" || name == "push" => {
                Some(DropKind::AfreeArr)
            }
            // A map literal (`[:]` / `["k": v]`) allocates a fresh Map (RFC-0028).
            Expr::MapLit { .. } => Some(DropKind::FreeMap),
            Expr::Call { name, .. } => self.owned.get(name).copied(),
            _ => None,
        }
    }

    /// Whether the binding `name` resolves to is a known `String`. The innermost
    /// one answers: an inner `let s = 1` under an outer `let s = "x"` is an
    /// integer, and reading the outer one there would free that integer.
    fn is_string_var(&self, name: &str) -> bool {
        self.str_vars.get(name).copied().unwrap_or(false)
    }

    /// A conservative, sound "is this expression a `String`?" test — used only to
    /// decide whether `a + b` is concatenation (heap) or arithmetic (no heap).
    /// It never reports a non-string as a string (so an integer add is never
    /// freed); when unsure it answers `false`, leaving a genuine string temporary
    /// to leak, which is always safe.
    fn expr_is_string(&self, e: &Expr) -> bool {
        match e {
            Expr::Str(_) => true,
            Expr::Call { name, .. } if name == "@concat" || name == "@str" => true,
            Expr::Call { name, .. } => self.string_fns.contains(name),
            Expr::Binary {
                op: BinOp::Add,
                lhs,
                ..
            } => self.expr_is_string(lhs),
            Expr::Var { name, .. } => self.is_string_var(name),
            _ => false,
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
                    self.str_vars.enter();
                    for n in pattern_binders(&arm.pattern) {
                        self.str_vars.bind(&n, false);
                    }
                    self.visit(&arm.body);
                    self.str_vars.exit();
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
                self.str_vars.enter();
                for p in params {
                    self.str_vars.bind(p, false);
                }
                match body {
                    LambdaBody::Expr(e2) => self.visit(e2),
                    LambdaBody::Block(b) => {
                        for s in &b.stmts {
                            self.stmt(s);
                        }
                    }
                }
                self.str_vars.exit();
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
            .map(|m| m.values().copied().collect())
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
        assert_eq!(drop_kinds(src, "main"), vec![DropKind::AfreeArr]);
    }

    #[test]
    fn explicitly_afreed_array_is_not_auto_freed() {
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = array(); \
                   a = push(a, 1); let v = at(a, 0); afree(a); return v; }";
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

    #[test]
    fn factory_returning_cell_is_owned() {
        let src =
            "fn make(v: Int64) -> Ref<Int64> { return cell(v); } fn main() -> Int64 { return 0; }";
        let (o, _) = analyze_src(src);
        assert_eq!(o.owned_fns.get("make"), Some(&DropKind::ReleaseRef));
    }
}
