//! Compile-time constant evaluation.
//!
//! Used to implement RFC-0003's rule: *if the compiler can prove a value, there
//! is no runtime cost.* The checker uses this to validate refinement predicates
//! against compile-time-constant arguments; the codegen backends use it to tell
//! whether a validated-type construction needs a runtime check at all.

use std::collections::HashMap;

use crate::ast::*;

/// A value known at compile time.
#[derive(Debug, Clone, PartialEq)]
pub enum ConstVal {
    Int(i64),
    Bool(bool),
    /// A float constant — participates in refinement predicates over a
    /// `Float`/`Float64` base with exact IEEE `f64` semantics (identical to
    /// both runtimes, so a compile-time proof never disagrees with them).
    Float(f64),
    /// A string constant — supports `value.length` and equality in refinement
    /// predicates over a `String` base (RFC-0003). Not `Copy` (owns its bytes).
    Str(String),
}

impl std::fmt::Display for ConstVal {
    /// The value as it would be written in source — used in diagnostics
    /// (`5 does not satisfy \`Age\``), never the `Debug` form (`Int(5)`).
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

/// Try to evaluate `expr` to a constant, given a constant environment (e.g.
/// `value` bound to a candidate during predicate checking). Returns `None` if
/// the expression is not a compile-time constant (contains a call, an unbound
/// variable, division by zero, etc.).
pub fn eval(expr: &Expr, env: &HashMap<String, ConstVal>) -> Option<ConstVal> {
    match expr {
        Expr::Int(n) => Some(ConstVal::Int(*n)),
        Expr::Bool(b) => Some(ConstVal::Bool(*b)),
        // String and float constants participate (for `String where` /
        // `Float where` refinements).
        Expr::Str(s) => Some(ConstVal::Str(s.clone())),
        Expr::Float(f) => Some(ConstVal::Float(*f)),
        Expr::Var { name, .. } => env.get(name).cloned(),
        Expr::Unary { op, expr, .. } => {
            let v = eval(expr, env)?;
            match (op, v) {
                // Wrapping: the language's defined overflow semantics (both
                // backends wrap), so a proof here never disagrees with runtime.
                (UnOp::Neg, ConstVal::Int(n)) => Some(ConstVal::Int(n.wrapping_neg())),
                (UnOp::Neg, ConstVal::Float(f)) => Some(ConstVal::Float(-f)),
                (UnOp::Not, ConstVal::Bool(b)) => Some(ConstVal::Bool(!b)),
                _ => None,
            }
        }
        Expr::Binary { op, lhs, rhs, .. } => {
            // short-circuit logical operators
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
            let l = eval(lhs, env)?;
            let r = eval(rhs, env)?;
            match (l, r) {
                (ConstVal::Int(a), ConstVal::Int(b)) => Some(match op {
                    // Wrapping two's complement — matches both runtimes exactly
                    // (checked_* would refuse to prove `value + 1 != 0` at
                    // i64::MAX, which the backends happily wrap). Division by
                    // zero and MIN/-1 stay unprovable: both trap at runtime.
                    BinOp::Add => ConstVal::Int(a.wrapping_add(b)),
                    BinOp::Sub => ConstVal::Int(a.wrapping_sub(b)),
                    BinOp::Mul => ConstVal::Int(a.wrapping_mul(b)),
                    BinOp::Div => ConstVal::Int(a.checked_div(b)?),
                    BinOp::Rem => ConstVal::Int(a.checked_rem(b)?),
                    BinOp::Lt => ConstVal::Bool(a < b),
                    BinOp::LtEq => ConstVal::Bool(a <= b),
                    BinOp::Gt => ConstVal::Bool(a > b),
                    BinOp::GtEq => ConstVal::Bool(a >= b),
                    BinOp::Eq => ConstVal::Bool(a == b),
                    BinOp::NotEq => ConstVal::Bool(a != b),
                    // Bitwise and/or/xor fold on the i64 representation — the
                    // result is width-independent for the stored value, so it
                    // agrees with every backend (RFC-0045).
                    BinOp::BitAnd => ConstVal::Int(a & b),
                    BinOp::BitOr => ConstVal::Int(a | b),
                    BinOp::BitXor => ConstVal::Int(a ^ b),
                    // Shifts and complement are width-dependent (arithmetic vs
                    // logical `>>`, complement mask), and consteval is
                    // type-unaware — leave them unfolded (the checker still
                    // const-rejects an out-of-range shift *amount*, which is a
                    // plain foldable number).
                    BinOp::Shl | BinOp::Shr => return None,
                    BinOp::And | BinOp::Or | BinOp::Match => return None,
                }),
                // IEEE f64 arithmetic — bit-identical in consteval, the
                // interpreter, and native doubles. `/ 0.0` is inf/NaN (IEEE),
                // never a trap. `%` on floats is rejected by the checker.
                (ConstVal::Float(a), ConstVal::Float(b)) => Some(match op {
                    BinOp::Add => ConstVal::Float(a + b),
                    BinOp::Sub => ConstVal::Float(a - b),
                    BinOp::Mul => ConstVal::Float(a * b),
                    BinOp::Div => ConstVal::Float(a / b),
                    BinOp::Lt => ConstVal::Bool(a < b),
                    BinOp::LtEq => ConstVal::Bool(a <= b),
                    BinOp::Gt => ConstVal::Bool(a > b),
                    BinOp::GtEq => ConstVal::Bool(a >= b),
                    BinOp::Eq => ConstVal::Bool(a == b),
                    BinOp::NotEq => ConstVal::Bool(a != b),
                    _ => return None,
                }),
                (ConstVal::Bool(a), ConstVal::Bool(b)) => match op {
                    BinOp::Eq => Some(ConstVal::Bool(a == b)),
                    BinOp::NotEq => Some(ConstVal::Bool(a != b)),
                    _ => None,
                },
                (ConstVal::Str(a), ConstVal::Str(b)) => match op {
                    BinOp::Eq => Some(ConstVal::Bool(a == b)),
                    BinOp::NotEq => Some(ConstVal::Bool(a != b)),
                    // `s =~ "pat"` compiles the (literal) pattern and full-matches.
                    BinOp::Match => crate::regex::compile(&b)
                        .ok()
                        .map(|dfa| ConstVal::Bool(dfa.matches(&a))),
                    _ => None,
                },
                _ => None,
            }
        }
        // `s.length` on a string constant folds to its byte length (matching the
        // native `strlen` and the interpreter's `Str::len`). Any other field access
        // is not a compile-time constant.
        Expr::Field { expr, field, .. } if field == "length" => match eval(expr, env)? {
            ConstVal::Str(s) => Some(ConstVal::Int(s.len() as i64)),
            _ => None,
        },
        // `s[i]` (which desugars to `at(s, i)`) folds to the byte value when both
        // the string and index are constants and the index is in bounds — this is
        // what lets a refinement predicate inspect individual characters.
        Expr::Call { name, args, .. } if name == "at" && args.len() == 2 => {
            match (eval(&args[0], env)?, eval(&args[1], env)?) {
                (ConstVal::Str(s), ConstVal::Int(i)) if i >= 0 => s
                    .as_bytes()
                    .get(i as usize)
                    .map(|b| ConstVal::Int(*b as i64)),
                _ => None,
            }
        }
        Expr::Call { .. } => None,
        // match / if-expr / ? / records / fallible construction are not constants
        // in v0.1.
        Expr::Match { .. }
        | Expr::IfExpr { .. }
        | Expr::Try { .. }
        | Expr::StructLit { .. }
        | Expr::Field { .. }
        | Expr::TryConstruct { .. }
        | Expr::ArrayLit { .. }
        | Expr::MapLit { .. }
        | Expr::Spawn { .. }
        | Expr::Lambda { .. } => None,
    }
}

/// True if `expr` contains any call (used to forbid calls in refinement
/// predicates, keeping them purely const-analyzable in v0.1).
pub fn contains_call(expr: &Expr) -> bool {
    match expr {
        Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) | Expr::Var { .. } => false,
        Expr::Unary { expr, .. } => contains_call(expr),
        Expr::Binary { lhs, rhs, .. } => contains_call(lhs) || contains_call(rhs),
        // Indexing (`s[i]` = `at(s, i)`) is a pure, const-foldable builtin, so it
        // is permitted in a refinement predicate; only its arguments are scanned.
        Expr::Call { name, args, .. } if name == "at" => args.iter().any(contains_call),
        Expr::Call { .. } => true,
        Expr::Match {
            scrutinee, arms, ..
        } => contains_call(scrutinee) || arms.iter().any(|a| contains_call(&a.body)),
        Expr::IfExpr {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            contains_call(cond)
                || contains_call(then_branch)
                || else_branch.as_ref().is_some_and(|e| contains_call(e))
        }
        Expr::Try { expr, .. } => contains_call(expr),
        Expr::StructLit { fields, .. } => fields.iter().any(|(_, e)| contains_call(e)),
        Expr::Field { expr, .. } => contains_call(expr),
        Expr::TryConstruct { args, .. } => args.iter().any(contains_call),
        Expr::ArrayLit { elems, .. } => elems.iter().any(contains_call),
        Expr::MapLit { entries, .. } => entries
            .iter()
            .any(|(k, v)| contains_call(k) || contains_call(v)),
        Expr::Spawn { .. } => true,
        // A lambda literal is not a constant and never appears in a refinement
        // predicate (the checker forbids it outside a call argument).
        Expr::Lambda { .. } => true,
    }
}
