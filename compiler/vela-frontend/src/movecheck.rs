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

use std::collections::{HashMap, HashSet};
use std::cell::RefCell;

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
        .map(|f| (f.name.clone(), f.params.iter().map(|p| p.capability).collect()))
        .collect();
    let mc = MoveCheck { caps: &caps, errors: RefCell::new(Vec::new()) };
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

    fn block(
        &self,
        b: &Block,
        consumed: &mut Consumed,
        scope: &mut Vec<HashSet<String>>,
    ) {
        scope.push(HashSet::new());
        for s in &b.stmts {
            if let Err(msg) = self.stmt(s, consumed, scope) {
                self.errors.borrow_mut().push(msg);
                // Keep going: the statement's sub-expression check ran before any
                // mutation, so `consumed`/`scope` are still consistent for the
                // next statement.
            }
        }
        scope.pop();
    }

    fn in_scope(scope: &[HashSet<String>], name: &str) -> bool {
        scope.iter().any(|f| f.contains(name))
    }

    fn stmt(
        &self,
        s: &Stmt,
        consumed: &mut Consumed,
        scope: &mut Vec<HashSet<String>>,
    ) -> Result<(), String> {
        match s {
            Stmt::Let { name, value, .. } => {
                self.expr(value, consumed, scope)?;
                consumed.remove(name); // a fresh binding is alive again
                scope.last_mut().unwrap().insert(name.clone());
                Ok(())
            }
            Stmt::Assign { name, value, .. } => {
                self.expr(value, consumed, scope)?;
                consumed.remove(name); // reassignment revives it
                Ok(())
            }
            Stmt::SetField { value, .. } => self.expr(value, consumed, scope),
            Stmt::Return { value, .. } => {
                if let Some(e) = value {
                    self.expr(e, consumed, scope)?;
                }
                Ok(())
            }
            Stmt::If { cond, then_block, else_block, .. } => {
                self.expr(cond, consumed, scope)?;
                let mut then_c = consumed.clone();
                self.block(then_block, &mut then_c, scope);
                let mut else_c = consumed.clone();
                if let Some(eb) = else_block {
                    self.block(eb, &mut else_c, scope);
                }
                // may-consume: consumed on either path ⇒ consumed afterward
                for (k, v) in then_c.into_iter().chain(else_c) {
                    consumed.entry(k).or_insert(v);
                }
                Ok(())
            }
            Stmt::While { cond, body, .. } => {
                // The condition re-runs on every iteration, so consumption in it
                // is loop-consumption exactly like the body's (`while take(x)`
                // would use `x` again next time around) — track both in the
                // in-loop map and run the same next-iteration check.
                let mut body_c = consumed.clone();
                self.expr(cond, &mut body_c, scope)?;
                self.block(body, &mut body_c, scope);
                for (k, (line, consumer)) in &body_c {
                    if !consumed.contains_key(k) && Self::in_scope(scope, k) {
                        return Err(format!(
                            "line {line}: `{k}` is consumed by {consumer} inside a loop, \
                             so it would be used again on the next iteration"
                        ));
                    }
                }
                for (k, v) in body_c {
                    consumed.entry(k).or_insert(v);
                }
                Ok(())
            }
            // A `for` loop consumes like a `while`: the iterable is read once,
            // and consuming an outer binding in the body is a use-again error.
            Stmt::ForIn { iter, body, .. } => {
                self.expr(iter, consumed, scope)?;
                let mut body_c = consumed.clone();
                self.block(body, &mut body_c, scope);
                for (k, (line, consumer)) in &body_c {
                    if !consumed.contains_key(k) && Self::in_scope(scope, k) {
                        return Err(format!(
                            "line {line}: `{k}` is consumed by {consumer} inside a loop, \
                             so it would be used again on the next iteration"
                        ));
                    }
                }
                for (k, v) in body_c {
                    consumed.entry(k).or_insert(v);
                }
                Ok(())
            }
            Stmt::Expr(e) => self.expr(e, consumed, scope),
            // A `region` is an ordinary nested block for move checking.
            Stmt::Region { body, .. } => {
                self.block(body, consumed, scope);
                Ok(())
            }
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
                Ok(())
            }
        }
    }

    fn expr(
        &self,
        e: &Expr,
        consumed: &mut Consumed,
        scope: &mut Vec<HashSet<String>>,
    ) -> Result<(), String> {
        match e {
            Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => Ok(()),
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
            Expr::Match { scrutinee, arms, .. } => {
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
            Expr::Call { name, args, line } => {
                let caps = self.caps.get(name);
                // Left-to-right: check each argument, then apply its consumption,
                // so passing the same variable to two `consume` params is caught.
                for (i, arg) in args.iter().enumerate() {
                    self.expr(arg, consumed, scope)?;
                    if caps.and_then(|c| c.get(i)) == Some(&Capability::Consume) {
                        if let Expr::Var { name: v, .. } = arg {
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
            // `spawn f(args)` moves arguments exactly like a direct call: a
            // `consume` parameter takes ownership across the task boundary.
            Expr::Spawn { name, args, line } => {
                let caps = self.caps.get(name);
                for (i, arg) in args.iter().enumerate() {
                    self.expr(arg, consumed, scope)?;
                    if caps.and_then(|c| c.get(i)) == Some(&Capability::Consume) {
                        if let Expr::Var { name: v, .. } = arg {
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
}

/// The payload names a `match` pattern binds.
fn pattern_bindings(p: &Pattern) -> Vec<&str> {
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
                                      let z = take(x); return join(t) + z; }";
        let e = run(src).unwrap_err();
        assert!(e.contains("already consumed by `spawn take(..)`"), "{e}");
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
