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
    for f in &program.functions {
        droppable.insert(
            f.name.clone(),
            analyze_fn(f, &owned, &string_fns, &string_types).droppable,
        );
    }
    // Test bodies (RFC-0015) get the same block-exit drop analysis so a `let` in
    // a test reclaims its heap value exactly as it would in a function. The body
    // is the REAL node the interpreter walks, so the by-address droppable keys
    // match at run time. Tests never return an owned value (they are `Unit`).
    for (i, t) in program.tests.iter().enumerate() {
        droppable.insert(
            format!("test@{i}"),
            analyze_body(&[], &t.body, &Type::Unit, &owned, &string_fns, &string_types).droppable,
        );
    }
    Ownership { owned_fns: owned, droppable }
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
    // Seed the string-var scope with any `String`-typed parameters.
    let params: HashSet<String> = params_list
        .iter()
        .filter(|p| is_string_like(&p.ty, string_types))
        .map(|p| p.name.clone())
        .collect();
    let mut a = Analysis {
        droppable: HashMap::new(),
        live: vec![HashMap::new()],
        region_depth: 0,
        owned,
        ret_is_heap: returns_owned_kind(ret).is_some(),
        all_returns_owned: true,
        string_fns,
        str_vars: vec![params],
    };
    a.block(body);
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
    /// Names of functions whose return type is a `String` (or a nominal type
    /// over `String`). Used to classify `a + b` as string concatenation when an
    /// operand is a call — a fresh heap String the caller then owns.
    string_fns: &'a HashSet<String>,
    /// Scope stack of `String`-typed variable names (params + string-bound lets).
    /// Kept in lock-step with `live`; lets `a + b` be recognised as a string
    /// concat (not integer arithmetic) without a full re-typing pass. It only
    /// ever under-approximates — an unrecognised string temporary is left to
    /// leak, never freed as if it were an integer.
    str_vars: Vec<HashSet<String>>,
}

impl Analysis<'_> {
    fn block(&mut self, b: &Block) {
        self.live.push(HashMap::new());
        self.str_vars.push(HashSet::new());
        for s in &b.stmts {
            self.stmt(s);
        }
        self.str_vars.pop();
        self.live.pop();
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Let { name, mutable, value, .. } => {
                // Account for uses in the initializer *before* the new binding
                // exists (so `let x = x + b` escapes the old `x`).
                self.visit(value);
                // Track a `String`-typed binding so later `a + b` on it is seen
                // as concatenation. Computed against the *pre-binding* env, so a
                // self-reference resolves to the old value's type.
                if self.expr_is_string(value) {
                    self.str_vars.last_mut().unwrap().insert(name.clone());
                }
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
            Expr::Call { name, .. } => self.owned.get(name).copied(),
            _ => None,
        }
    }

    /// Whether `name` is a known `String`-typed binding in the current scopes.
    fn is_string_var(&self, name: &str) -> bool {
        self.str_vars.iter().any(|f| f.contains(name))
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
            Expr::Binary { op: BinOp::Add, lhs, .. } => self.expr_is_string(lhs),
            Expr::Var { name, .. } => self.is_string_var(name),
            _ => false,
        }
    }

    /// Walk an expression, escaping any candidate used outside a safe read.
    fn visit(&mut self, e: &Expr) {
        match e {
            Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => {}
            Expr::Var { name, .. } => self.escape(name),
            Expr::Unary { expr, .. } | Expr::Try { expr, .. } => self.visit(expr),
            // `x.length` reads the length header only — a safe read of a candidate
            // (matches `s.length` replacing `len(s)`). Any other field access is a
            // conservative escape.
            Expr::Field { expr, field, .. } if field == "length" => self.operand(expr),
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
            // A lambda body (RFC-0023): a captured heap binding is passed by value
            // into the monomorphized lambda function, which never frees it (the
            // enclosing scope keeps ownership). Walking the body conservatively
            // treats a captured candidate as escaped, so it is not auto-freed at
            // the capture site — sound (never a double-free; at worst a leak, which
            // does not affect observable behavior or parity).
            Expr::Lambda { body, .. } => match body {
                LambdaBody::Expr(e2) => self.visit(e2),
                LambdaBody::Block(b) => {
                    for s in &b.stmts {
                        self.stmt(s);
                    }
                }
            },
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
        o.droppable.get(which).map(|m| m.values().copied().collect()).unwrap_or_default()
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

    #[test]
    fn factory_returning_cell_is_owned() {
        let src = "fn make(v: Int64) -> Ref<Int64> { return cell(v); } fn main() -> Int64 { return 0; }";
        let (o, _) = analyze_src(src);
        assert_eq!(o.owned_fns.get("make"), Some(&DropKind::ReleaseRef));
    }
}
