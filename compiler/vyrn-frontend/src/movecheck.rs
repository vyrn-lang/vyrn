//! Move checking for the `consume` capability (RFC-0004).
//!
//! A `consume` parameter takes ownership of its argument: after a variable is
//! passed to one, using it again is an error. This is the first, tractable slice
//! of the capability model — ownership expressed as *intent* (`consume`) and
//! enforced by the compiler, rather than through `&`/move mechanics. It runs as
//! a separate pass after type checking, so the type checker stays unaware of it.
//!
//! `Read`/`Modify`/`Share` impose no restriction in v0.1 (they are surface-only);
//! only `Consume` moves. Analysis is flow-sensitive: `if` merges branches with
//! "may-consume" (a value consumed on either path is consumed afterward), a
//! reassignment revives a variable, and consuming a pre-loop variable inside a
//! loop body is rejected (it would be reused next iteration).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::diagnostics::Diagnostic;

/// Check every function for use-after-consume, returning **all** problems found
/// as structured [`Diagnostic`]s. Each function is checked independently, so
/// a use-after-consume error in one function does not suppress errors in others.
/// Within a function, errors accumulate at **statement boundaries** (the same
/// RFC-0006 model as the type checker): `block` push-and-continues, so two
/// independent consume bugs in one body are both reported. A statement's
/// internals still use `?`, so within a single statement (and a single expression)
/// the first error wins — this is sound because every statement does its
/// sub-expression checking *before* mutating `consumed`/`scope`, so after an
/// error the flow state is consistent for the next statement.
pub fn check_accum(program: &Program) -> Vec<Diagnostic> {
    let caps: HashMap<String, Vec<Capability>> = program
        .functions
        .iter()
        .map(|f| {
            (
                f.name.clone(),
                f.params.iter().map(|p| p.capability).collect(),
            )
        })
        .collect();
    let globals: HashSet<String> = program.globals.iter().map(|g| g.name.clone()).collect();
    let mc = MoveCheck {
        caps: &caps,
        globals: &globals,
        errors: RefCell::new(Vec::new()),
    };
    let mut out = Vec::new();
    for f in &program.functions {
        mc.errors.borrow_mut().clear();
        mc.function(f);
        for s in mc.errors.borrow_mut().drain(..) {
            let mut d = Diagnostic::from_rendered(s, "movecheck");
            d.file = f.module.clone();
            out.push(d);
        }
    }
    // Test bodies (RFC-0015) move-check as ordinary Unit function bodies under a
    // synthetic name, so use-after-consume inside a test is caught unchanged.
    for (i, t) in program.tests.iter().enumerate() {
        let synthetic = Function {
            name: format!("test@{i}"),
            exported: false,
            module: t.module.clone(),
            doc: None,
            type_params: Vec::new(),
            type_bounds: Default::default(),
            params: Vec::new(),
            ret: Type::Unit,
            body: t.body.clone(),
            line: t.line,
            is_extern: false,
            is_export_extern: false,
            is_gen: false,
        };
        mc.errors.borrow_mut().clear();
        mc.function(&synthetic);
        for s in mc.errors.borrow_mut().drain(..) {
            let mut d = Diagnostic::from_rendered(s, "movecheck");
            d.file = t.module.clone();
            out.push(d);
        }
    }
    // Bench bodies (RFC-0055) move-check identically, under a synthetic `bench@`
    // name.
    for (i, b) in program.benches.iter().enumerate() {
        let synthetic = Function {
            name: format!("bench@{i}"),
            exported: false,
            module: b.module.clone(),
            doc: None,
            type_params: Vec::new(),
            type_bounds: Default::default(),
            params: Vec::new(),
            ret: Type::Unit,
            body: b.body.clone(),
            line: b.line,
            is_extern: false,
            is_export_extern: false,
            is_gen: false,
        };
        mc.errors.borrow_mut().clear();
        mc.function(&synthetic);
        for s in mc.errors.borrow_mut().drain(..) {
            let mut d = Diagnostic::from_rendered(s, "movecheck");
            d.file = b.module.clone();
            out.push(d);
        }
    }
    out
}

/// Check every function for use-after-consume. Runs after type checking. Returns
/// the first problem found (rendered as the historical `"line {N}: {message}"`
/// string). Thin shim over [`check_accum`].
pub fn check(program: &Program) -> Result<(), String> {
    match check_accum(program).into_iter().next() {
        Some(d) => Err(d.render()),
        None => Ok(()),
    }
}

struct MoveCheck<'a> {
    caps: &'a HashMap<String, Vec<Capability>>,
    /// Module-state binding names (RFC-0013). A global may never be passed to a
    /// `consume` parameter — nothing may take ownership of module state.
    globals: &'a HashSet<String>,
    /// Per-function statement-boundary error sink (RFC-0006 accumulation).
    /// Cleared at the start of each function, drained by `check_accum`.
    errors: RefCell<Vec<String>>,
}

/// Consumed variables: name -> (line consumed, description of the consumer).
type Consumed = HashMap<String, (usize, String)>;

impl MoveCheck<'_> {
    fn function(&self, f: &Function) {
        let mut consumed: Consumed = HashMap::new();
        let mut scope: Vec<HashSet<String>> =
            vec![f.params.iter().map(|p| p.name.clone()).collect()];
        self.block(&f.body, &mut consumed, &mut scope);
    }

    /// Returns whether this block **diverges** — every path out of it leaves via
    /// `return`/`break`/`continue` (RFC-0060). A statement after a diverging one
    /// is unreachable, so it is not checked (use-after-move there is not an
    /// error), and its consumptions never flow to the block's exit.
    fn block(&self, b: &Block, consumed: &mut Consumed, scope: &mut Vec<HashSet<String>>) -> bool {
        scope.push(HashSet::new());
        let mut diverged = false;
        for s in &b.stmts {
            if diverged {
                // Unreachable after a `return`/`break`/`continue`: skip it
                // (the `return` precedent — code after it is unreachable-clean).
                break;
            }
            match self.stmt(s, consumed, scope) {
                Ok(d) => diverged = d,
                Err(msg) => {
                    self.errors.borrow_mut().push(msg);
                    // Keep going: the statement's sub-expression check ran before
                    // any mutation, so state is consistent for the next statement.
                }
            }
        }
        scope.pop();
        diverged
    }

    fn in_scope(scope: &[HashSet<String>], name: &str) -> bool {
        scope.iter().any(|f| f.contains(name))
    }

    /// Returns whether this statement **diverges** (leaves via
    /// `return`/`break`/`continue` on every path) — see [`MoveCheck::block`].
    fn stmt(
        &self,
        s: &Stmt,
        consumed: &mut Consumed,
        scope: &mut Vec<HashSet<String>>,
    ) -> Result<bool, String> {
        match s {
            Stmt::Let { name, value, .. } => {
                self.expr(value, consumed, scope)?;
                consumed.remove(name); // a fresh binding is alive again
                scope.last_mut().unwrap().insert(name.clone());
                Ok(false)
            }
            Stmt::Assign { name, value, .. } => {
                self.expr(value, consumed, scope)?;
                consumed.remove(name); // reassignment revives it
                Ok(false)
            }
            Stmt::SetField { value, .. } => self.expr(value, consumed, scope).map(|_| false),
            // `a[i] = v` — the stored value is consumed like a `push` argument
            // (neither `push` nor the store marks it consumed, since no user
            // `consume` capability is involved), so just check both sub-exprs.
            Stmt::IndexSet { index, value, .. } => {
                self.expr(index, consumed, scope)?;
                self.expr(value, consumed, scope)?;
                Ok(false)
            }
            Stmt::Return { value, .. } => {
                if let Some(e) = value {
                    self.expr(e, consumed, scope)?;
                }
                Ok(true)
            }
            // `break`/`continue` (RFC-0060) consume nothing but terminate the
            // path — code after them in the same block is unreachable.
            Stmt::Break { .. } | Stmt::Continue { .. } => Ok(true),
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                self.expr(cond, consumed, scope)?;
                let mut then_c = consumed.clone();
                let then_div = self.block(then_block, &mut then_c, scope);
                let mut else_c = consumed.clone();
                let else_div = match else_block {
                    Some(eb) => self.block(eb, &mut else_c, scope),
                    None => false,
                };
                // may-consume, but a branch that DIVERGES (break/continue/return)
                // carries its consumptions out the exit path, not to the code
                // after the `if` — so a value moved only on a break-path is not
                // considered moved on the fall-through (RFC-0060).
                if !then_div {
                    for (k, v) in then_c {
                        consumed.entry(k).or_insert(v);
                    }
                }
                if !else_div {
                    for (k, v) in else_c {
                        consumed.entry(k).or_insert(v);
                    }
                }
                Ok(then_div && else_div)
            }
            // `if let PAT = e { .. } else { .. }` (RFC-0060): the scrutinee is
            // consumed eagerly (like a `match` scrutinee), the binders are fresh
            // locals of the then-arm, and the two arms merge exactly like `if` —
            // a branch that diverges carries its consumptions out, not through.
            Stmt::IfLet {
                pattern,
                scrutinee,
                then_block,
                else_block,
                ..
            } => {
                self.expr(scrutinee, consumed, scope)?;
                let mut then_c = consumed.clone();
                scope.push(HashSet::new());
                for b in pattern_bindings(pattern) {
                    scope.last_mut().unwrap().insert(b.to_string());
                }
                let then_div = self.block(then_block, &mut then_c, scope);
                scope.pop();
                let mut else_c = consumed.clone();
                let else_div = match else_block {
                    Some(eb) => self.block(eb, &mut else_c, scope),
                    None => false,
                };
                if !then_div {
                    for (k, v) in then_c {
                        consumed.entry(k).or_insert(v);
                    }
                }
                if !else_div {
                    for (k, v) in else_c {
                        consumed.entry(k).or_insert(v);
                    }
                }
                Ok(then_div && else_div)
            }
            Stmt::While { cond, body, .. } => {
                // The condition re-runs on every iteration, so consumption in it
                // is loop-consumption exactly like the body's (`while take(x)`
                // would use `x` again next time around) — track both in the
                // in-loop map and run the same next-iteration check.
                let mut body_c = consumed.clone();
                self.expr(cond, &mut body_c, scope)?;
                let body_div = self.block(body, &mut body_c, scope);
                self.check_loop_reuse(consumed, &body_c, scope, body_div)?;
                for (k, v) in body_c {
                    consumed.entry(k).or_insert(v);
                }
                Ok(false)
            }
            // A `for` loop consumes like a `while`: the iterable is read once,
            // and consuming an outer binding in the body is a use-again error.
            Stmt::ForIn { iter, body, .. } => {
                self.expr(iter, consumed, scope)?;
                let mut body_c = consumed.clone();
                let body_div = self.block(body, &mut body_c, scope);
                self.check_loop_reuse(consumed, &body_c, scope, body_div)?;
                for (k, v) in body_c {
                    consumed.entry(k).or_insert(v);
                }
                Ok(false)
            }
            Stmt::Expr(e) => self.expr(e, consumed, scope).map(|_| false),
            // A `region` is an ordinary nested block for move checking; it
            // diverges iff its body does (a `break` inside it exits the loop).
            Stmt::Region { body, .. } => Ok(self.block(body, consumed, scope)),
            // `drop name;` consumes the binding: using it afterward is a
            // use-after-drop, caught by the same machinery as `consume`.
            Stmt::Drop { name, line } => {
                if let Some((cline, consumer)) = consumed.get(name) {
                    return Err(format!(
                        "line {line}: `{name}` is dropped here but was already consumed by \
                         {consumer} on line {cline}"
                    ));
                }
                consumed.insert(name.clone(), (*line, "`drop`".to_string()));
                Ok(false)
            }
        }
    }

    /// The loop-body reuse check: a variable consumed in the body (`body_c`) that
    /// was live before the loop would be consumed *again* on the next iteration.
    /// Skipped when the body **diverges unconditionally** (`body_div`) — it then
    /// runs at most once (a straight-line `consume(x); break`), so the
    /// consumption is legal and flows out to the enclosing scope instead.
    fn check_loop_reuse(
        &self,
        consumed: &Consumed,
        body_c: &Consumed,
        scope: &[HashSet<String>],
        body_div: bool,
    ) -> Result<(), String> {
        if body_div {
            return Ok(());
        }
        for (k, (line, consumer)) in body_c {
            if !consumed.contains_key(k) && Self::in_scope(scope, k) {
                return Err(format!(
                    "line {line}: `{k}` is consumed by {consumer} inside a loop, \
                     so it would be used again on the next iteration"
                ));
            }
        }
        Ok(())
    }

    fn expr(
        &self,
        e: &Expr,
        consumed: &mut Consumed,
        scope: &mut Vec<HashSet<String>>,
    ) -> Result<(), String> {
        match e {
            Expr::Int(_) | Expr::Byte(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => Ok(()),
            Expr::Var { name, line } => {
                if let Some((cline, consumer)) = consumed.get(name) {
                    return Err(format!(
                        "line {line}: `{name}` is used here but was already consumed by {consumer} \
                         on line {cline}\n  (a `consume` parameter takes ownership; the value can't \
                         be used afterward)"
                    ));
                }
                Ok(())
            }
            Expr::Unary { expr, .. } => self.expr(expr, consumed, scope),
            Expr::Binary { lhs, rhs, .. } => {
                self.expr(lhs, consumed, scope)?;
                self.expr(rhs, consumed, scope)
            }
            Expr::Field { expr, .. } => self.expr(expr, consumed, scope),
            Expr::Try { expr, .. } => self.expr(expr, consumed, scope),
            Expr::StructLit { fields, .. } => {
                for (_, v) in fields {
                    self.expr(v, consumed, scope)?;
                }
                Ok(())
            }
            Expr::TryConstruct { args, .. } => {
                for a in args {
                    self.expr(a, consumed, scope)?;
                }
                Ok(())
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.expr(scrutinee, consumed, scope)?;
                let base = consumed.clone();
                let mut merged: Option<Consumed> = None;
                for arm in arms {
                    let mut c = base.clone();
                    scope.push(HashSet::new());
                    for b in pattern_bindings(&arm.pattern) {
                        scope.last_mut().unwrap().insert(b.to_string());
                    }
                    self.expr(&arm.body, &mut c, scope)?;
                    scope.pop();
                    match &mut merged {
                        None => merged = Some(c),
                        Some(m) => {
                            for (k, v) in c {
                                m.entry(k).or_insert(v);
                            }
                        }
                    }
                }
                if let Some(m) = merged {
                    *consumed = m;
                }
                Ok(())
            }
            // `if` as an expression (RFC-0030): its two branches are match arms —
            // the condition consumes eagerly, then each branch runs from the same
            // base and a value consumed on either path is may-consumed afterward.
            Expr::IfExpr {
                cond,
                then_branch,
                else_branch,
                ..
            } => {
                self.expr(cond, consumed, scope)?;
                let base = consumed.clone();
                let mut then_c = base.clone();
                self.expr(then_branch, &mut then_c, scope)?;
                let mut else_c = base;
                if let Some(eb) = else_branch {
                    self.expr(eb, &mut else_c, scope)?;
                }
                for (k, v) in then_c.into_iter().chain(else_c) {
                    consumed.entry(k).or_insert(v);
                }
                Ok(())
            }
            Expr::Call { name, args, line } => {
                let caps = self.caps.get(name);
                // Left-to-right: check each argument, then apply its consumption,
                // so passing the same variable to two `consume` params is caught.
                for (i, arg) in args.iter().enumerate() {
                    self.expr(arg, consumed, scope)?;
                    if caps.and_then(|c| c.get(i)) == Some(&Capability::Consume) {
                        if let Expr::Var { name: v, line: vl } = arg {
                            if !Self::in_scope(scope, v) {
                                self.reject_consume_global(v, name, false, *vl)?;
                            }
                            consumed
                                .entry(v.clone())
                                .or_insert((*line, format!("`{name}(..)`")));
                        }
                    }
                }
                Ok(())
            }
            Expr::ArrayLit { elems, .. } => {
                for e in elems {
                    self.expr(e, consumed, scope)?;
                }
                Ok(())
            }
            Expr::MapLit { entries, .. } => {
                for (k, v) in entries {
                    self.expr(k, consumed, scope)?;
                    self.expr(v, consumed, scope)?;
                }
                Ok(())
            }
            // A lambda body (RFC-0023): its untyped params are fresh locals; walk
            // the body so a `consume`-misuse inside it is still caught. Captured
            // bindings are read-only (the checker forbids consuming/dropping them),
            // so a reference to one that was already consumed surfaces the standard
            // use-after-consume error here too.
            Expr::Lambda { params, body, .. } => {
                scope.push(HashSet::new());
                for p in params {
                    scope.last_mut().unwrap().insert(p.clone());
                }
                let r = match body {
                    LambdaBody::Expr(inner) => self.expr(inner, consumed, scope),
                    LambdaBody::Block(b) => {
                        self.block(b, consumed, scope);
                        Ok(())
                    }
                };
                scope.pop();
                r
            }
            // `spawn f(args)` moves arguments exactly like a direct call: a
            // `consume` parameter takes ownership across the task boundary.
            Expr::Spawn { name, args, line } => {
                let caps = self.caps.get(name);
                for (i, arg) in args.iter().enumerate() {
                    self.expr(arg, consumed, scope)?;
                    if caps.and_then(|c| c.get(i)) == Some(&Capability::Consume) {
                        if let Expr::Var { name: v, line: vl } = arg {
                            if !Self::in_scope(scope, v) {
                                self.reject_consume_global(v, name, true, *vl)?;
                            }
                            consumed
                                .entry(v.clone())
                                .or_insert((*line, format!("`spawn {name}(..)`")));
                        }
                    }
                }
                Ok(())
            }
        }
    }

    /// Reject passing a module-state binding to a `consume` parameter (RFC-0013):
    /// nothing may take ownership of module state. A local of the same name is
    /// tracked in `scope` elsewhere; this only fires when `v` is genuinely a
    /// global. The `scope` shadowing check is done by the caller having already
    /// excluded locals — here we only know the name is a global if it is in the
    /// global set AND not shadowed, which the type checker's scope resolves; for
    /// move checking a global is never in `scope`'s binder sets, so membership in
    /// `globals` alone (when not a param/let) is decisive.
    fn reject_consume_global(
        &self,
        v: &str,
        callee: &str,
        spawned: bool,
        line: usize,
    ) -> Result<(), String> {
        if self.globals.contains(v) {
            let form = if spawned {
                format!("spawn {callee}(..)")
            } else {
                format!("{callee}(..)")
            };
            return Err(format!(
                "line {line}: module state `{v}` may not be passed to a `consume` parameter \
                 via `{form}` — nothing may take ownership of module state (it lives for the \
                 whole module and is never dropped)"
            ));
        }
        Ok(())
    }
}

/// The payload names a `match` pattern binds.
pub fn pattern_bindings(p: &Pattern) -> Vec<&str> {
    match p {
        Pattern::Some(b) | Pattern::Ok(b) | Pattern::Err(b) => vec![b],
        Pattern::Variant(_, binds) => binds.iter().map(|s| s.as_str()).collect(),
        Pattern::None => vec![],
    }
}

#[cfg(test)]
mod tests {
    fn run(src: &str) -> Result<(), String> {
        let program = crate::parser::parse(crate::lexer::lex(src).unwrap()).unwrap();
        super::check(&program)
    }

    #[test]
    fn rejects_use_after_consume() {
        let src = "type T = { id: Int64 }; \
                   fn use_up(t: consume T) -> Int64 { return t.id; } \
                   fn main() -> Int64 { let x = T { id: 1 }; let a = use_up(x); return use_up(x); }";
        let e = run(src).unwrap_err();
        assert!(e.contains("already consumed"), "{e}");
    }

    #[test]
    fn rejects_use_after_consume_inside_a_test_body() {
        // RFC-0015: a test body is move-checked exactly like a function body.
        let src = "type T = { id: Int64 }; \
                   fn use_up(t: consume T) -> Int64 { return t.id; } \
                   test \"consumes twice\" { let x = T { id: 1 }; \
                       let a = use_up(x); let b = use_up(x); assert(a == b) }";
        let e = run(src).unwrap_err();
        assert!(e.contains("already consumed"), "{e}");
    }

    #[test]
    fn rejects_smallarray_use_after_drop() {
        // RFC-0056: a moved-from `SmallArray` is dead (move copies the whole
        // struct incl. inline slots, but movecheck semantics are unchanged) —
        // using it after `drop` is rejected, exactly like any owned value.
        let src = "fn main() -> Int64 { \
                   let mut xs: SmallArray<Int64, 4> = []  xs.push(1)  \
                   drop xs  return xs.length }";
        let e = run(src).unwrap_err();
        assert!(e.contains("consumed") || e.contains("drop"), "{e}");
    }

    #[test]
    fn allows_read_reuse() {
        let src = "type T = { id: Int64 }; \
                   fn peek(t: read T) -> Int64 { return t.id; } \
                   fn main() -> Int64 { let x = T { id: 1 }; return peek(x) + peek(x); }";
        assert!(run(src).is_ok());
    }

    #[test]
    fn consume_then_no_reuse_is_ok() {
        let src = "type T = { id: Int64 }; \
                   fn take(t: consume T) -> Int64 { return t.id; } \
                   fn main() -> Int64 { let x = T { id: 1 }; return take(x); }";
        assert!(run(src).is_ok());
    }

    #[test]
    fn reassignment_revives() {
        let src = "type T = { id: Int64 }; \
                   fn take(t: consume T) -> Int64 { return t.id; } \
                   fn main() -> Int64 { let mut x = T { id: 1 }; let a = take(x); \
                                      x = T { id: 2 }; return a + take(x); }";
        assert!(run(src).is_ok());
    }

    #[test]
    fn rejects_consume_in_while_condition() {
        // The condition re-runs every iteration — consuming there is the same
        // bug as consuming in the body.
        let src = "type T = { id: Int64 }; \
                   fn take(t: consume T) -> Bool { return t.id > 0; } \
                   fn main() -> Int64 { let x = T { id: 1 }; \
                                      while take(x) { let y = 1; } return 0; }";
        let e = run(src).unwrap_err();
        assert!(e.contains("inside a loop"), "{e}");
    }

    #[test]
    fn spawn_applies_consume_capabilities() {
        // `spawn take(x)` moves x across the task boundary; a second use is a
        // double move.
        let src = "type T = { id: Int64 }; \
                   fn take(t: consume T) -> Int64 { return t.id; } \
                   fn main() -> Int64 { let x = T { id: 1 }; \
                                      let t = spawn take(x); \
                                      let z = take(x); return t.join() + z; }";
        let e = run(src).unwrap_err();
        assert!(e.contains("already consumed by `spawn take(..)`"), "{e}");
    }

    #[test]
    fn rejects_passing_global_to_consume_param() {
        // RFC-0013: nothing may take ownership of module state.
        let src = "type T = { id: Int64 } \
                   let g = T { id: 1 } \
                   fn take(t: consume T) -> Int64 { return t.id; } \
                   fn use_it() -> Int64 { return take(g); } \
                   fn main() -> Int64 { return 0; }";
        let e = run(src).unwrap_err();
        assert!(e.contains("module state") && e.contains("consume"), "{e}");
    }

    #[test]
    fn local_shadowing_global_may_be_consumed() {
        // A local `g` shadows the global, so consuming it is fine.
        let src = "type T = { id: Int64 } \
                   let g = T { id: 1 } \
                   fn take(t: consume T) -> Int64 { return t.id; } \
                   fn use_it() -> Int64 { let g = T { id: 2 } return take(g); } \
                   fn main() -> Int64 { return 0; }";
        assert!(run(src).is_ok(), "{:?}", run(src));
    }

    #[test]
    fn break_path_consume_rejected_after_loop() {
        // Consumed on the way out of the loop, then used after it — rejected.
        let src = "type T = { id: Int64 }; \
                   fn take(t: consume T) -> Int64 { return t.id; } \
                   fn main() -> Int64 { let x = T { id: 1 }; \
                       for i in [0, 1] { let a = take(x); break } \
                       return take(x); }";
        let e = run(src).unwrap_err();
        assert!(e.contains("already consumed"), "{e}");
    }

    #[test]
    fn consume_on_break_branch_not_moved_on_fall_through() {
        // `x` is consumed only on the branch that breaks; the fall-through path
        // never consumed it, so a later read in the same body is fine (RFC-0060).
        let src = "type T = { id: Int64 }; \
                   fn take(t: consume T) -> Int64 { return t.id; } \
                   fn peek(t: read T) -> Int64 { return t.id; } \
                   fn main() -> Int64 { let x = T { id: 1 }; let mut s = 0; \
                       for i in [0, 1, 2] { \
                           if i == 2 { let a = take(x); break } \
                           s = s + peek(x) } \
                       return s; }";
        assert!(run(src).is_ok(), "{:?}", run(src));
    }

    #[test]
    fn use_after_break_is_unreachable_clean() {
        // The second `take(x)` is after an unconditional `break` — unreachable, so
        // it is not a use-after-consume (RFC-0060: code after break is dead).
        let src = "type T = { id: Int64 }; \
                   fn take(t: consume T) -> Int64 { return t.id; } \
                   fn main() -> Int64 { let x = T { id: 1 }; \
                       while true { break let a = take(x); let b = take(x); } \
                       return 0; }";
        assert!(run(src).is_ok(), "{:?}", run(src));
    }

    #[test]
    fn rejects_consume_in_loop() {
        let src = "type T = { id: Int64 }; \
                   fn take(t: consume T) -> Int64 { return t.id; } \
                   fn main() -> Int64 { let x = T { id: 1 }; let mut i = 0; \
                                      while i < 3 { let a = take(x); i = i + 1; } return 0; }";
        let e = run(src).unwrap_err();
        assert!(e.contains("inside a loop"), "{e}");
    }
}
