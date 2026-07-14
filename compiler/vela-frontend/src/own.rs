//! Ownership / drop analysis for heap temporaries (RFC-0004 §4).
//!
//! This is the *ownership* half of the memory model's Path A — the counterpart
//! to `region` arenas. It decides, per function, two things:
//!
//!   * **droppable** `let` bindings — ones that own a fresh heap allocation and
//!     provably do not escape their block, so the backend frees them at block
//!     exit; and
//!   * whether the function **returns an owned value** — every heap-typed return
//!     hands the caller a fresh, unaliased allocation, transferring ownership out
//!     so the *caller's* receiving binding becomes droppable in turn.
//!
//! A fresh heap value is produced by `concat(..)` or by a call to a function that
//! itself returns owned (computed by fixpoint over the call graph). A binding is
//! droppable unless it is `mut`, lexically inside a `region` (the arena owns it),
//! or *escapes*: it appears anywhere except as a whole argument of
//! `len`/`print`/`concat` (which only read a string) or an operand of `==`/`!=`.
//! Returning a local owner is a *move* (the value leaves, so it is not dropped
//! here); aliasing it (`let t = x`) or passing it to any other function escapes
//! it. Anything not provably single-owned is simply left to leak — always safe,
//! never a use-after-free or double-free.
//!
//! Identities are `Stmt::Let` node addresses (`*const Stmt as usize`): the
//! backend runs this on the same borrowed AST it emits, so the addresses match
//! one-to-one — a collision-free key where a source line is not (two `let`s can
//! share a line). Because a non-region `concat` uses `malloc` and a region one
//! uses the arena, and this analysis skips the region case, the two reclamation
//! mechanisms partition every allocation — nothing is freed twice.

use std::collections::HashMap;

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
}

/// Whole-program ownership facts.
pub struct Ownership {
    /// Functions whose return value transfers heap ownership to the caller,
    /// with the kind of value returned.
    pub owned_fns: HashMap<String, DropKind>,
    /// Per function: identity of each droppable `let` and how to reclaim it.
    pub droppable: HashMap<String, HashMap<usize, DropKind>>,
}

/// Analyse ownership across a whole program.
pub fn analyze(program: &Program) -> Ownership {
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
            if snapshot.contains_key(&f.name) && !analyze_fn(f, &snapshot).is_owned {
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
    for f in &program.functions {
        droppable.insert(f.name.clone(), analyze_fn(f, &owned).droppable);
    }
    Ownership { owned_fns: owned, droppable }
}

/// The reclamation kind a function transfers to its caller, if its return type is
/// a heap value the caller then owns. `String` → free; `Ref` → release. Nominal
/// string types and records-with-strings are left out for now (they leak — safe).
fn returns_owned_kind(ty: &Type) -> Option<DropKind> {
    match ty {
        Type::Str => Some(DropKind::FreeStr),
        Type::Ref(_) => Some(DropKind::ReleaseRef),
        Type::Array(_) => Some(DropKind::AfreeArr),
        _ => None,
    }
}

struct FnResult {
    droppable: HashMap<usize, DropKind>,
    is_owned: bool,
}

fn analyze_fn(f: &Function, owned: &HashMap<String, DropKind>) -> FnResult {
    let mut a = Analysis {
        droppable: HashMap::new(),
        live: vec![HashMap::new()],
        region_depth: 0,
        owned,
        ret_is_heap: returns_owned_kind(&f.ret).is_some(),
        all_returns_owned: true,
    };
    a.block(&f.body);
    FnResult { droppable: a.droppable, is_owned: a.ret_is_heap && a.all_returns_owned }
}

/// The identity key for a statement: its node address.
fn id(s: &Stmt) -> usize {
    s as *const Stmt as usize
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
}

impl Analysis<'_> {
    fn block(&mut self, b: &Block) {
        self.live.push(HashMap::new());
        for s in &b.stmts {
            self.stmt(s);
        }
        self.live.pop();
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Let { name, mutable, value, .. } => {
                // Account for uses in the initializer *before* the new binding
                // exists (so `let x = concat(x, b)` escapes the old `x`).
                self.visit(value);
                if let Some(kind) = self.owner_producing(value) {
                    // A dynamic string inside a region is owned by the arena, so
                    // skip it. A cell (`ReleaseRef`) lives in the separate slab,
                    // which the region does not touch, so release it regardless.
                    let region_owns = kind == DropKind::FreeStr && self.region_depth > 0;
                    // Arrays are reassigned in place (`a = push(a, x)`), so a
                    // `mut` array can still own a buffer; strings/refs must be
                    // single-assignment to be tracked.
                    let assignable_ok = !*mutable || kind == DropKind::AfreeArr;
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
                    if let Expr::Call { name: fname, args, .. } = value {
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
            Stmt::Return { value, .. } => self.ret(value.as_ref()),
            Stmt::If { cond, then_block, else_block, .. } => {
                self.visit(cond);
                self.block(then_block);
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
            Stmt::ForIn { iter, body, .. } => {
                self.visit(iter);
                self.block(body);
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
    /// `concat` → a string, `cell` → a reference, or a call to an owned function
    /// (its declared kind). Otherwise `None`.
    fn owner_producing(&self, e: &Expr) -> Option<DropKind> {
        match e {
            Expr::Call { name, .. } if name == "concat" || name == "str" => Some(DropKind::FreeStr),
            Expr::Call { name, .. } if name == "cell" => Some(DropKind::ReleaseRef),
            Expr::Call { name, .. } if name == "array" || name == "push" => {
                Some(DropKind::AfreeArr)
            }
            Expr::Call { name, .. } => self.owned.get(name).copied(),
            _ => None,
        }
    }

    /// Walk an expression, escaping any candidate used outside a safe read.
    fn visit(&mut self, e: &Expr) {
        match e {
            Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => {}
            Expr::Var { name, .. } => self.escape(name),
            Expr::Unary { expr, .. } | Expr::Field { expr, .. } | Expr::Try { expr, .. } => {
                self.visit(expr)
            }
            Expr::Binary { op, lhs, rhs, .. } => {
                if matches!(op, BinOp::Eq | BinOp::NotEq) {
                    self.operand(lhs);
                    self.operand(rhs);
                } else {
                    self.visit(lhs);
                    self.visit(rhs);
                }
            }
            Expr::Call { name, args, .. } => {
                // These builtins only *read* their heap argument and never retain
                // it — a whole candidate passed to one is a safe use: `len` /
                // `print` / `concat` for strings, `get` for references,
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
                    "len" | "print" | "concat" | "get" | "at" | "alen"
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
            Expr::Match { scrutinee, arms, .. } => {
                self.visit(scrutinee);
                for arm in arms {
                    self.visit(&arm.body);
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
            Expr::Spawn { args, .. } => {
                for e in args {
                    self.visit(e);
                }
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
        let src = "fn main() -> Int { let a = \"x\"; let b = \"y\"; \
                   let s = concat(a, b); let n = len(s); return n; }";
        assert_eq!(drop_count(src, "main"), 1);
    }

    #[test]
    fn does_not_free_aliased_temporary() {
        let src = "fn main() -> Int { let a = \"x\"; let b = \"y\"; \
                   let s = concat(a, b); let t = s; return len(t); }";
        assert_eq!(drop_count(src, "main"), 0);
    }

    #[test]
    fn concat_argument_is_a_safe_read() {
        let src = "fn main() -> Int { let a = \"x\"; let b = \"y\"; \
                   let s = concat(a, b); let u = concat(s, b); return len(u); }";
        assert_eq!(drop_count(src, "main"), 2);
    }

    #[test]
    fn set_value_argument_escapes() {
        // `set(c, s)` stores `s` in the cell, which outlives the block — `s`
        // must NOT stay droppable (auto-freeing it would leave the cell
        // dangling; the next `get` would be a use-after-free).
        let src = "fn main() -> Int { let a = \"x\"; let b = \"y\"; \
                   let c = cell(\"seed\"); \
                   if true { let s = concat(a, b); set(c, s); } \
                   print(get(c)); release(c); return 0; }";
        assert_eq!(drop_count(src, "main"), 0);
    }

    #[test]
    fn set_ref_argument_is_a_safe_read() {
        // Passing an owned *cell* to `set`/`get` does not escape the cell
        // binding — with no explicit `release`, it stays auto-releasable.
        let src = "fn main() -> Int { let c = cell(1); set(c, 2); \
                   let n = get(c); return n; }";
        assert_eq!(drop_count(src, "main"), 1);
    }

    #[test]
    fn skips_temporary_inside_region() {
        let src = "fn main() -> Int { let a = \"x\"; let b = \"y\"; let mut n = 0; \
                   region { let s = concat(a, b); n = len(s); } return n; }";
        assert_eq!(drop_count(src, "main"), 0);
    }

    #[test]
    fn skips_mutable_binding() {
        let src = "fn main() -> Int { let a = \"x\"; let b = \"y\"; \
                   let mut s = concat(a, b); return len(s); }";
        assert_eq!(drop_count(src, "main"), 0);
    }

    // ---- ownership transfer ---------------------------------------------

    #[test]
    fn factory_returning_concat_is_owned() {
        let src = "fn make(a: String, b: String) -> String { return concat(a, b); } \
                   fn main() -> Int { return 0; }";
        let (o, _) = analyze_src(src);
        assert!(o.owned_fns.contains_key("make"));
    }

    #[test]
    fn factory_returning_local_owner_is_owned_and_moves_it() {
        let src = "fn make(a: String, b: String) -> String { let s = concat(a, b); return s; } \
                   fn main() -> Int { return 0; }";
        let (o, _) = analyze_src(src);
        assert!(o.owned_fns.contains_key("make"));
        // `s` is moved out by the return, so it is not dropped inside `make`.
        assert_eq!(o.droppable.get("make").map(|s| s.len()).unwrap_or(0), 0);
    }

    #[test]
    fn identity_returning_param_is_not_owned() {
        let src = "fn id(s: String) -> String { return s; } fn main() -> Int { return 0; }";
        let (o, _) = analyze_src(src);
        assert!(!o.owned_fns.contains_key("id"));
    }

    #[test]
    fn mixed_return_paths_are_not_owned() {
        let src = "fn pick(c: Bool, a: String, b: String) -> String { \
                       if c { return concat(a, b); } return a; } \
                   fn main() -> Int { return 0; }";
        let (o, _) = analyze_src(src);
        assert!(!o.owned_fns.contains_key("pick"));
    }

    #[test]
    fn caller_frees_owned_call_result() {
        // `y` receives a fresh owned value from `make` and doesn't escape.
        let src = "fn make(a: String, b: String) -> String { return concat(a, b); } \
                   fn main() -> Int { let a = \"x\"; let b = \"y\"; \
                       let y = make(a, b); return len(y); }";
        let (o, _) = analyze_src(src);
        assert_eq!(o.droppable.get("main").map(|s| s.len()).unwrap_or(0), 1);
    }

    #[test]
    fn caller_does_not_free_borrowed_call_result() {
        // `id` is not owned, so its result must not be freed by the caller.
        let src = "fn id(s: String) -> String { return s; } \
                   fn main() -> Int { let a = \"x\"; let b = \"y\"; \
                       let s = concat(a, b); let y = id(s); return len(y); }";
        let (o, _) = analyze_src(src);
        // `s` escapes into the `id(..)` call, `y` is not an owned result:
        assert_eq!(o.droppable.get("main").map(|s| s.len()).unwrap_or(0), 0);
    }

    // ---- inferred release for references --------------------------------

    fn drop_kinds(src: &str, which: &str) -> Vec<DropKind> {
        let (o, _) = analyze_src(src);
        o.droppable.get(which).map(|m| m.values().copied().collect()).unwrap_or_default()
    }

    #[test]
    fn non_escaping_cell_is_auto_released() {
        let src = "fn main() -> Int { let c = cell(1); set(c, get(c) + 1); return get(c); }";
        assert_eq!(drop_kinds(src, "main"), vec![DropKind::ReleaseRef]);
    }

    #[test]
    fn aliased_cell_is_not_auto_released() {
        // `c` is aliased into `d`, so it must not be auto-released.
        let src = "fn main() -> Int { let c = cell(1); let d = c; return get(d); }";
        assert_eq!(drop_count(src, "main"), 0);
    }

    #[test]
    fn explicitly_released_cell_is_not_auto_released() {
        // Passing `c` to `release` hands the cell off — no auto-release on top,
        // which would double-release and trap.
        let src = "fn main() -> Int { let c = cell(1); let v = get(c); release(c); return v; }";
        assert_eq!(drop_count(src, "main"), 0);
    }

    #[test]
    fn cell_inside_region_is_still_released() {
        // The cell slab is separate from the arena, so a region does not reclaim
        // it — ownership still auto-releases the reference.
        let src = "fn main() -> Int { let mut n = 0; \
                   region { let c = cell(7); n = get(c); } return n; }";
        assert_eq!(drop_kinds(src, "main"), vec![DropKind::ReleaseRef]);
    }

    // ---- auto-free for mutable arrays -----------------------------------

    #[test]
    fn mut_array_with_self_update_is_auto_freed() {
        let src = "fn main() -> Int { let mut a: Array<Int> = array(); \
                   let mut i = 0; while i < 3 { a = push(a, i); i = i + 1; } \
                   return at(a, 0); }";
        assert_eq!(drop_kinds(src, "main"), vec![DropKind::AfreeArr]);
    }

    #[test]
    fn explicitly_afreed_array_is_not_auto_freed() {
        let src = "fn main() -> Int { let mut a: Array<Int> = array(); \
                   a = push(a, 1); let v = at(a, 0); afree(a); return v; }";
        assert_eq!(drop_count(src, "main"), 0);
    }

    #[test]
    fn returned_array_is_not_auto_freed() {
        let src = "fn build() -> Array<Int> { let mut a: Array<Int> = array(); \
                   a = push(a, 1); return a; } fn main() -> Int { return 0; }";
        // `a` is moved out by the return, so it is not freed inside `build`.
        assert_eq!(drop_count(src, "build"), 0);
    }

    #[test]
    fn factory_returning_cell_is_owned() {
        let src = "fn make(v: Int) -> Ref<Int> { return cell(v); } fn main() -> Int { return 0; }";
        let (o, _) = analyze_src(src);
        assert_eq!(o.owned_fns.get("make"), Some(&DropKind::ReleaseRef));
    }
}
