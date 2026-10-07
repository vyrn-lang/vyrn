//! The constructor of a `where` type: a value of a validated type
//! exists only through its producer.
//!
//! The predicate becomes ordinary Vyrn, generated per declaration and injected
//! into the linked program, so every backend compiles one body. Each
//! declaration gets two functions:
//!
//! - [`pred_name`]: `fn(binds..) -> Bool`, whose body is the `where` clause,
//!   with [`crate::types::predicate_binds`] as parameters (a record base binds
//!   every field, any other base binds `value`).
//! - [`ctor_name`]: `fn(value: Base)`, which calls the predicate and `panic`s
//!   with [`crate::trap::validation_of`]'s sentence when it fails.
//!
//! A fallible construction (`Age?(n)`) needs the predicate without the trap,
//! hence two functions. The constructor returns `Unit`: a validated value's
//! representation is its base, which the caller already holds, and returning
//! `value` would cross into the validated type and call the constructor again.
//! It uses `panic`, not a trap-table row, because the wording names the
//! declaration; the loader stamps no site onto it (see [`crate::loader`]'s
//! runtime-module rule), so every engine prints the same bytes.

use std::collections::HashMap;

use crate::ast::{Block, Expr, Function, Id, Param, Stmt, Type, TypeDecl, UnOp};

/// The reserved prefix of both generated names. `$` is no identifier
/// character, so no program can spell or shadow one.
pub const PREFIX: &str = "where$";

/// The prefix of every predicate function's name.
pub const PRED_PREFIX: &str = "where$p";

/// Returns the name of `decl`'s predicate function: `fn(binds..) -> Bool`.
pub fn pred_name(name: &str) -> String {
    format!("{PRED_PREFIX}{name}")
}

/// Returns the name of `decl`'s constructor: `fn(value: Base)`, which traps.
pub fn ctor_name(name: &str) -> String {
    format!("{PREFIX}c{name}")
}

/// Returns the arguments of a call to [`pred_name`], given the expression for
/// the whole value: each field of a record base, or the value itself, as
/// [`crate::types::predicate_binds`] lists them.
fn pred_args(decl: &TypeDecl, value: Expr) -> Vec<Expr> {
    crate::types::predicate_binds(decl)
        .into_iter()
        .map(|(name, _, field)| match field {
            Some(_) => Expr::field(value.clone(), name, 0),
            None => value.clone(),
        })
        .collect()
}

/// Returns the predicate and constructor of every declaration with a `where`,
/// to append to a linked program. `check_and_synthesize` calls it before any
/// backend builds its function table; afterwards they are ordinary functions
/// to every pass.
pub fn constructors(types: &HashMap<String, TypeDecl>) -> Vec<Function> {
    let mut names: Vec<&String> = types.keys().collect();
    names.sort();
    let mut out = Vec::new();
    for n in names {
        let decl = &types[n];
        if decl.predicate.is_none() {
            continue;
        }
        // A generic validated type has no single base: its predicate is written over
        // type parameters. The backends check it at the boundary instead.
        if !decl.type_params.is_empty() {
            continue;
        }
        out.push(predicate_fn(decl));
        out.push(constructor_fn(decl));
    }
    out
}

/// `fn where$p<Name>(binds..) -> Bool { return <the where clause> }`, whose
/// body is the declaration's own predicate node.
fn predicate_fn(decl: &TypeDecl) -> Function {
    Function::synth(
        pred_name(&decl.name),
        crate::types::predicate_binds(decl)
            .into_iter()
            .map(|(name, ty, _)| Param::synth(name, ty))
            .collect(),
        Type::Bool,
        vec![Stmt::ret(
            decl.predicate.clone().expect("predicate present"),
            0,
        )],
    )
}

/// `fn where$c<Name>(value: Base) { if !where$p<Name>(..) { panic("..") } }`.
fn constructor_fn(decl: &TypeDecl) -> Function {
    let value = Expr::var("value", 0);
    let holds = Expr::call(pred_name(&decl.name), pred_args(decl, value), 0);
    let fail = Stmt::expr(Expr::call(
        "panic",
        vec![Expr::str(crate::trap::validation_of(decl))],
        0,
    ));
    Function::synth(
        ctor_name(&decl.name),
        vec![Param::synth("value", decl.base.clone())],
        Type::Unit,
        vec![Stmt::If {
            id: Id::NEW,
            cond: Expr::Unary {
                id: Id::NEW,
                op: UnOp::Not,
                expr: Box::new(holds),
                line: 0,
            },
            then_block: Block {
                id: Id::NEW,
                stmts: vec![fail],
            },
            else_block: None,
            line: 0,
        }],
    )
}
