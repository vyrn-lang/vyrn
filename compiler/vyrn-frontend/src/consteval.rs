//! Compile-time constant evaluation, for the rule: if the compiler can
//! prove a value, there is no runtime cost. The checker validates refinement
//! predicates against constant arguments with it, and the backends ask whether
//! a validated-type construction needs a runtime check.

use std::collections::HashMap;

use crate::ast::*;
use crate::prim::Cmp;

/// A value known at compile time.
#[derive(Debug, Clone, PartialEq)]
pub enum ConstVal {
    Int(i64),
    Bool(bool),
    /// A float constant, with IEEE `f64` semantics identical to the runtimes, so a
    /// proof never disagrees with them.
    Float(f64),
    /// A string constant: supports `value.byteLength`, equality and `=~` in
    /// refinement predicates.
    Str(String),
}

impl std::fmt::Display for ConstVal {
    /// Writes the value as source spells it, for diagnostics
    /// (`5 does not satisfy `Age``).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConstVal::Int(n) => write!(f, "{n}"),
            ConstVal::Bool(b) => write!(f, "{b}"),
            ConstVal::Float(x) => write!(f, "{x}"),
            ConstVal::Str(s) => write!(f, "{s:?}"),
        }
    }
}

impl ConstVal {
    pub fn as_bool(self) -> Option<bool> {
        match self {
            ConstVal::Bool(b) => Some(b),
            _ => None,
        }
    }
}

/// Evaluates `expr` to a constant in the environment `env` (such as `value`
/// bound to a candidate). `None` if it is not a compile-time constant: a call,
/// an unbound variable, a division by zero.
pub fn eval(expr: &Expr, env: &HashMap<String, ConstVal>) -> Option<ConstVal> {
    match expr {
        Expr::Int(n, _) => Some(ConstVal::Int(*n)),
        // A byte literal folds to its integer, so a predicate can inspect
        // bytes (`value[0] == 'H'`).
        Expr::Byte(b, _) => Some(ConstVal::Int(*b as i64)),
        Expr::Bool(b, _) => Some(ConstVal::Bool(*b)),
        Expr::Str(s, _) => Some(ConstVal::Str(s.clone())),
        Expr::Float(f, _) => Some(ConstVal::Float(*f)),
        Expr::Var { name, .. } => env.get(name).cloned(),
        Expr::Unary { op, expr, .. } => {
            let v = eval(expr, env)?;
            match (op, v) {
                // Wrapping is the language's overflow semantics, as at run time.
                (UnOp::Neg, ConstVal::Int(n)) => Some(ConstVal::Int(n.wrapping_neg())),
                (UnOp::Neg, ConstVal::Float(f)) => Some(ConstVal::Float(-f)),
                (UnOp::Not, ConstVal::Bool(b)) => Some(ConstVal::Bool(!b)),
                _ => None,
            }
        }
        Expr::Binary { op, lhs, rhs, .. } => {
            match op {
                BinOp::And => {
                    return match eval(lhs, env)?.as_bool()? {
                        false => Some(ConstVal::Bool(false)),
                        true => Some(ConstVal::Bool(eval(rhs, env)?.as_bool()?)),
                    }
                }
                BinOp::Or => {
                    return match eval(lhs, env)?.as_bool()? {
                        true => Some(ConstVal::Bool(true)),
                        false => Some(ConstVal::Bool(eval(rhs, env)?.as_bool()?)),
                    }
                }
                _ => {}
            }
            let (l, r) = (eval(lhs, env)?, eval(rhs, env)?);
            let row = op.row();
            if let Some(c) = row.cmp {
                // Only equality folds on booleans and strings.
                let equal = matches!(c, Cmp::Equal { .. });
                let o = match (l, r) {
                    (ConstVal::Int(a), ConstVal::Int(b)) => Some(a.cmp(&b)),
                    (ConstVal::Float(a), ConstVal::Float(b)) => a.partial_cmp(&b),
                    (ConstVal::Bool(a), ConstVal::Bool(b)) if equal => Some(a.cmp(&b)),
                    (ConstVal::Str(a), ConstVal::Str(b)) if equal => Some(a.cmp(&b)),
                    _ => return None,
                };
                return Some(ConstVal::Bool(c.holds(o)));
            }
            match (l, r) {
                (ConstVal::Int(a), ConstVal::Int(b)) => (row.int)(a, b).map(ConstVal::Int),
                (ConstVal::Float(a), ConstVal::Float(b)) => (row.float)(a, b).map(ConstVal::Float),
                // `s =~ "pat"` full-matches the literal pattern.
                (ConstVal::Str(a), ConstVal::Str(b)) if *op == BinOp::Match => {
                    crate::regex::compile(&b)
                        .ok()
                        .map(|dfa| ConstVal::Bool(dfa.matches(&a)))
                }
                _ => None,
            }
        }
        // `s.byteLength` on a string constant folds to its byte length. No other
        // field access is a constant.
        Expr::Field { expr, field, .. } if field == "byteLength" => match eval(expr, env)? {
            ConstVal::Str(s) => Some(ConstVal::Int(s.len() as i64)),
            _ => None,
        },
        // `s[i]` (`@at(s, i)`) folds to the byte when both are constants and the
        // index is in bounds.
        Expr::Call { name, args, .. } if name == crate::project::AT && args.len() == 2 => {
            match (eval(&args[0], env)?, eval(&args[1], env)?) {
                (ConstVal::Str(s), ConstVal::Int(i)) if i >= 0 => s
                    .as_bytes()
                    .get(i as usize)
                    .map(|b| ConstVal::Int(*b as i64)),
                _ => None,
            }
        }
        Expr::Call { .. } => None,
        Expr::Match { .. }
        | Expr::IfExpr { .. }
        | Expr::Try { .. }
        | Expr::StructLit { .. }
        | Expr::Field { .. }
        | Expr::TryConstruct { .. }
        | Expr::ArrayLit { .. }
        | Expr::MapLit { .. }
        // A consume is a move, never a constant.
        | Expr::Consume { .. }
        | Expr::Lambda { .. } => None,
    }
}

// The descent over a body is `ast::body_scope_descent!`'s.
crate::body_scope_descent!(BodyVisit, body_block, body_stmt, body_expr);

/// Records whether a site holds something not const-analyzable.
struct Calls(bool);

impl BodyVisit<'_> for Calls {
    const SCOPED: bool = false;

    fn expr(&mut self, e: &Expr, _: &std::collections::HashSet<String>) -> bool {
        match e {
            // `@at` is a pure, foldable builtin, allowed in a refinement predicate;
            // only its arguments are scanned.
            Expr::Call { name, .. } if name == crate::project::AT => {}
            // A lambda and a block match arm never appear in a predicate;
            // both count as a call, so a hole refuses instead of folding.
            Expr::Call { .. } | Expr::Lambda { .. } => self.0 = true,
            Expr::Match { arms, .. } if arms.iter().any(|a| a.body.as_expr().is_none()) => {
                self.0 = true
            }
            _ => {}
        }
        !self.0
    }
}

/// Returns whether `expr` contains a call; refinement predicates forbid calls
/// to stay const-analyzable.
pub fn contains_call(expr: &Expr) -> bool {
    let mut v = Calls(false);
    body_expr(expr, &std::collections::HashSet::new(), &mut v);
    v.0
}

/// Records each name a predicate reads other than through its length.
struct Whole(std::collections::HashSet<String>);

impl BodyVisit<'_> for Whole {
    const SCOPED: bool = false;

    fn expr(&mut self, e: &Expr, _: &std::collections::HashSet<String>) -> bool {
        match e {
            Expr::Field { expr, field, .. }
                if matches!(&**expr, Expr::Var { .. })
                    && (field == "length" || field == "byteLength") =>
            {
                false
            }
            Expr::Var { name, .. } => {
                self.0.insert(name.clone());
                true
            }
            _ => true,
        }
    }
}

/// The names `expr` reads other than as `name.length` or `name.byteLength`.
/// A store into an element of an array field outside them keeps the rule.
pub fn whole_reads(expr: &Expr) -> std::collections::HashSet<String> {
    let mut v = Whole(std::collections::HashSet::new());
    body_expr(expr, &std::collections::HashSet::new(), &mut v);
    v.0
}
