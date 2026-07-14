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
    /// A string constant — supports `value.length` and equality in refinement
    /// predicates over a `String` base (RFC-0003). Not `Copy` (owns its bytes).
    Str(String),
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
        // String constants participate (for `String where` refinements); floats
        // are still not const-evaluated in v0.1.
        Expr::Str(s) => Some(ConstVal::Str(s.clone())),
        Expr::Float(_) => None,
        Expr::Var { name, .. } => env.get(name).cloned(),
        Expr::Unary { op, expr, .. } => {
            let v = eval(expr, env)?;
            match (op, v) {
                (UnOp::Neg, ConstVal::Int(n)) => Some(ConstVal::Int(n.checked_neg()?)),
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
                    BinOp::Add => ConstVal::Int(a.checked_add(b)?),
                    BinOp::Sub => ConstVal::Int(a.checked_sub(b)?),
                    BinOp::Mul => ConstVal::Int(a.checked_mul(b)?),
                    BinOp::Div => ConstVal::Int(a.checked_div(b)?),
                    BinOp::Rem => ConstVal::Int(a.checked_rem(b)?),
                    BinOp::Lt => ConstVal::Bool(a < b),
                    BinOp::LtEq => ConstVal::Bool(a <= b),
                    BinOp::Gt => ConstVal::Bool(a > b),
                    BinOp::GtEq => ConstVal::Bool(a >= b),
                    BinOp::Eq => ConstVal::Bool(a == b),
                    BinOp::NotEq => ConstVal::Bool(a != b),
                    BinOp::And | BinOp::Or | BinOp::Match => return None,
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
                    BinOp::Match => crate::regex::compile(&b).ok().map(|dfa| ConstVal::Bool(dfa.matches(&a))),
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
                (ConstVal::Str(s), ConstVal::Int(i)) if i >= 0 => {
                    s.as_bytes().get(i as usize).map(|b| ConstVal::Int(*b as i64))
                }
                _ => None,
            }
        }
        Expr::Call { .. } => None,
        // match / ? / records / fallible construction are not constants in v0.1.
        Expr::Match { .. }
        | Expr::Try { .. }
        | Expr::StructLit { .. }
        | Expr::Field { .. }
        | Expr::TryConstruct { .. }
        | Expr::ArrayLit { .. }
        | Expr::Spawn { .. } => None,
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
        Expr::Match { scrutinee, arms, .. } => {
            contains_call(scrutinee) || arms.iter().any(|a| contains_call(&a.body))
        }
        Expr::Try { expr, .. } => contains_call(expr),
        Expr::StructLit { fields, .. } => fields.iter().any(|(_, e)| contains_call(e)),
        Expr::Field { expr, .. } => contains_call(expr),
        Expr::TryConstruct { args, .. } => args.iter().any(contains_call),
        Expr::ArrayLit { elems, .. } => elems.iter().any(contains_call),
        Expr::Spawn { .. } => true,
    }
}
