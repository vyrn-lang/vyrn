//! The constructor of a `where` type — RFC-0125 §2.3, §3 M6 (the third
//! judgment's fourth slice).
//!
//! §2.3 says a value of a validated type exists only through its producer, and
//! the census of §3 M6 sorted `where-scalar` and `where-record` into that line.
//! Before this module the two rows had three carriers: the interpreter ran the
//! predicate over a `Val`, the textual emitter lowered it to LLVM, and the
//! direct wasm backend lowered it again to wasm. One sentence, three
//! statements, and the only thing that held them together was
//! [`crate::types::predicate_binds`] plus the wording in [`crate::trap`].
//!
//! Here the predicate becomes **ordinary Vyrn**, generated per declaration and
//! injected into the linked program the way [`crate::jsonenc`] and
//! [`crate::jsondec`] inject their walks (RFC-0078 M2b, M3). All three engines
//! then run one body, which is the mechanism the census named: an exported
//! function of the program is a thing the interpreter interprets and both
//! emitters compile, and it is why `char-boundary` and `json-decode` are the
//! census's one-copy rows.
//!
//! # Two functions, not one
//!
//! - [`pred_name`] — `fn(binds..) -> Bool`, whose body IS the `where` clause.
//!   Its parameters are [`crate::types::predicate_binds`], so a record base
//!   binds every field and every other base binds `value` (RFC-0003).
//! - [`ctor_name`] — `fn(value: Base)`, which calls the predicate and `panic`s
//!   with [`crate::trap::validation_of`]'s sentence when it does not hold.
//!
//! The pair rather than one function, because a FALLIBLE construction
//! (`Age?(n)`, RFC-0077 M2k) wants the same answer without the trap. Two
//! spellings of "run the predicate" could disagree about what `value` means;
//! one function called by both cannot.
//!
//! # Why the constructor answers nothing
//!
//! It takes the raw value and returns Unit. A validated value's runtime
//! representation IS its base — `Interp::construct`'s own words, "zero
//! overhead" — so the caller already holds what the constructor would answer,
//! and "the type answers the value" is the caller keeping the value it passed
//! in. Returning it would cost twice: the constructor's own `return value`
//! crosses into the validated type, which is the boundary that runs the
//! predicate, so the function would call itself for ever; and a record base
//! would have to be moved out and back for a check that cannot write to it.
//!
//! # Why `panic` and not a trap table row
//!
//! The wording is per DECLARATION — `` validation failed for `Age` `` — so
//! there is no fixed row to index. [`crate::trap::validation_of`] is still the
//! one place the sentence is spelled; this module puts it in a `String`
//! literal of the generated body, and the loader deliberately does not stamp a
//! site onto it (see [`crate::loader`]'s runtime-module rule), so every engine
//! prints the census's exact bytes.

use std::collections::HashMap;

use crate::ast::{Block, Capability, Expr, Function, Param, Stmt, Type, TypeDecl, UnOp};

/// The reserved prefix of both generated names. `$` is not an identifier
/// character, so no program can spell one or shadow one — the defence
/// [`crate::loader::RT_PREFIX`] states for the JSON runtime.
pub const PREFIX: &str = "where$";

/// The name of `decl`'s predicate function: `fn(binds..) -> Bool`.
pub fn pred_name(name: &str) -> String {
    format!("{PREFIX}p{name}")
}

/// The name of `decl`'s constructor: `fn(value: Base)`, which traps.
pub fn ctor_name(name: &str) -> String {
    format!("{PREFIX}c{name}")
}

/// The arguments a call to [`pred_name`] takes, given the expression that
/// stands for the whole value.
///
/// A record base binds every field by name, so the call reads them off the
/// value; every other base binds `value` and the call passes it through. The
/// LIST is [`crate::types::predicate_binds`]'s, which is what keeps this from
/// becoming a second opinion about what a predicate sees.
pub fn pred_args(decl: &TypeDecl, value: Expr) -> Vec<Expr> {
    crate::types::predicate_binds(decl)
        .into_iter()
        .map(|(name, _, field)| match field {
            Some(_) => Expr::Field {
                expr: Box::new(value.clone()),
                field: name,
                line: 0,
            },
            None => value.clone(),
        })
        .collect()
}

/// The predicate and constructor for every declaration of `program` that
/// carries a `where`, ready to be appended to a linked program.
///
/// Called from `check_and_synthesize` beside the JSON walks and for the same
/// reason: the checker has just typed the program, and no engine has built its
/// function table yet. Afterwards these are ordinary Vyrn — move-checked with
/// everything else and lowered by every backend as source it cannot tell apart
/// from the user's.
pub fn constructors(types: &HashMap<String, TypeDecl>) -> Vec<Function> {
    let mut names: Vec<&String> = types.keys().collect();
    names.sort();
    let mut out = Vec::new();
    for n in names {
        let decl = &types[n];
        if decl.predicate.is_none() {
            continue;
        }
        // A GENERIC validated type has no single base to build over: its
        // predicate is written against type parameters the declaration does not
        // fix. The engines still check it at the boundary, so the row keeps its
        // per-engine statement there; the census records that as the remaining
        // exception rather than this file guessing an instantiation.
        if !decl.type_params.is_empty() {
            continue;
        }
        out.push(predicate_fn(decl));
        out.push(constructor_fn(decl));
    }
    out
}

/// `fn where$p<Name>(binds..) -> Bool { return <the `where` clause> }`.
///
/// The body is the declaration's own predicate node, so the check every engine
/// now calls is the expression the user wrote, once.
fn predicate_fn(decl: &TypeDecl) -> Function {
    synth(
        pred_name(&decl.name),
        crate::types::predicate_binds(decl)
            .into_iter()
            .map(|(name, ty, _)| Param {
                name,
                capability: Capability::Read,
                ty,
            })
            .collect(),
        Type::Bool,
        vec![Stmt::Return {
            value: Some(decl.predicate.clone().expect("predicate present")),
            line: 0,
        }],
    )
}

/// `fn where$c<Name>(value: Base) { if !where$p<Name>(..) { panic("..") } }`.
fn constructor_fn(decl: &TypeDecl) -> Function {
    let value = Expr::Var {
        name: "value".to_string(),
        line: 0,
    };
    let holds = Expr::Call {
        name: pred_name(&decl.name),
        args: pred_args(decl, value),
        line: 0,
    };
    let fail = Stmt::Expr(Expr::Call {
        name: "panic".to_string(),
        args: vec![Expr::Str(crate::trap::validation_of(decl))],
        line: 0,
    });
    synth(
        ctor_name(&decl.name),
        vec![Param {
            name: "value".to_string(),
            capability: Capability::Read,
            ty: decl.base.clone(),
        }],
        Type::Unit,
        vec![Stmt::If {
            cond: Expr::Unary {
                op: UnOp::Not,
                expr: Box::new(holds),
                line: 0,
            },
            then_block: Block { stmts: vec![fail] },
            else_block: None,
            line: 0,
        }],
    )
}

/// One synthesized function, with every field a generated body has no source
/// for set the way [`crate::jsondec`] sets them.
fn synth(name: String, params: Vec<Param>, ret: Type, stmts: Vec<Stmt>) -> Function {
    Function {
        name,
        exported: false,
        module: None,
        doc: None,
        type_params: Vec::new(),
        type_bounds: HashMap::new(),
        params,
        ret,
        body: Block { stmts },
        line: 0,
        col: 0,
        is_extern: false,
        is_export_extern: false,
        is_gen: false,
        is_mut: false,
    }
}
