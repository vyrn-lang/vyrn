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

use std::collections::{HashMap, HashSet};

use crate::ast::{
    BinOp, Block, ContractMemberKind, Expr, Function, Id, Numbering, Param, Program, Stmt, Type,
    TypeDecl, UnOp,
};
use crate::types::Decls;

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

/// The declaration of the union of `members`, each a validated scalar type
/// over one base: that base, the `||` of the members' rules, and the members.
fn union_decl(name: String, members: Vec<Type>, types: &dyn Decls) -> TypeDecl {
    let rule = |m: &Type| match m {
        Type::Named(n) => types.decl(n).and_then(|d| d.predicate.clone()),
        _ => None,
    };
    let predicate = members
        .iter()
        .filter_map(rule)
        .reduce(|lhs, rhs| Expr::Binary {
            op: BinOp::Or,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
            line: 0,
            id: Id::NEW,
        });
    TypeDecl {
        name,
        exported: false,
        export_to: Vec::new(),
        module: None,
        doc: None,
        type_params: Vec::new(),
        base: crate::types::resolve(&members[0], types),
        predicate,
        members,
        line: 0,
    }
}

/// Makes each union `program` writes a validated type before the checker
/// runs, so every pass checks it as one: `Port | Age` becomes the name of a
/// declaration [`union_decl`] writes, and `type Open = Port | Age` becomes
/// that declaration itself. A union of one member is that member, and one
/// whose base absorbed the rest is the base. A union that forms none
/// ([`crate::types::canonical_union`]) stays, for the checker to refuse.
/// Running it again names only what the last run did not.
pub fn name_unions(program: &mut Program) {
    // A program that writes no union pays one walk and no allocation.
    let mut seen = false;
    each_written_type(program, &mut |t| seen |= matches!(t, Type::Union(_)));
    if !seen {
        return;
    }
    let types = crate::types::decl_map(program);
    let canonical = |ms: &[Type]| crate::types::canonical_union(ms, &types).ok();
    // Numbered after the program's own units, as `Program::number_appended`
    // numbers an appended function.
    let mut units = program.units;
    let mut number = |mut d: TypeDecl| {
        if let Some(p) = &mut d.predicate {
            Numbering::unit(units).expr(p);
            units += 1;
        }
        d
    };
    for d in program
        .type_decls
        .iter_mut()
        .filter(|d| d.predicate.is_none())
    {
        let Type::Union(ms) = &d.base else { continue };
        match canonical(ms) {
            Some(Type::Union(ms)) if ms.len() > 1 => {
                let u = number(union_decl(d.name.clone(), ms, &types));
                (d.base, d.predicate, d.members) = (u.base, u.predicate, u.members);
            }
            Some(Type::Union(mut ms)) => d.base = ms.remove(0),
            Some(base) => d.base = base,
            None => {}
        }
    }
    let mut made: Vec<TypeDecl> = Vec::new();
    each_written_type(program, &mut |t| {
        let Some(u) = (match t {
            Type::Union(ms) => canonical(ms),
            _ => None,
        }) else {
            return;
        };
        *t = match u {
            Type::Union(ms) if ms.len() > 1 => {
                let n = Type::Union(ms.clone()).to_string();
                if types.get(&n).is_none() && !made.iter().any(|d| d.name == n) {
                    made.push(number(union_decl(n.clone(), ms, &types)));
                }
                Type::Named(n)
            }
            Type::Union(mut ms) => ms.remove(0),
            base => base,
        };
    });
    program.units = units;
    program.type_decls.extend(made);
}

/// Calls `f` on every type `program` writes, and on each part of it, outer
/// first: a declaration's base, a signature, an annotation and a call's type
/// arguments.
fn each_written_type(program: &mut Program, f: &mut dyn FnMut(&mut Type)) {
    crate::body_scope_descent!(TypeSites, sites_block, sites_stmt, sites_expr, mut);

    struct Sites<'f>(&'f mut dyn FnMut(&mut Type));

    impl TypeSites for Sites<'_> {
        const SCOPED: bool = false;

        fn stmt(&mut self, s: &mut Stmt, _: &HashSet<String>) {
            if let Stmt::Let { ty: Some(t), .. } = s {
                crate::types::walk_type_mut(t, &mut self.0);
            }
        }

        fn expr(&mut self, e: &mut Expr, _: &HashSet<String>) -> bool {
            if let Expr::Call { type_args, .. } = e {
                for t in type_args {
                    crate::types::walk_type_mut(t, &mut self.0);
                }
            }
            true
        }
    }

    let mut sites = Sites(f);
    let walk = |t: &mut Type, sites: &mut Sites| crate::types::walk_type_mut(t, &mut sites.0);
    for d in &mut program.type_decls {
        walk(&mut d.base, &mut sites);
    }
    let fns = (program.impls.iter_mut()).flat_map(|i| i.methods.iter_mut().chain(&mut i.places));
    for f in program.functions.iter_mut().chain(fns) {
        for p in &mut f.params {
            walk(&mut p.ty, &mut sites);
        }
        walk(&mut f.ret, &mut sites);
        sites_block(&mut f.body, &mut HashSet::new(), &mut sites);
    }
    for i in &mut program.impls {
        walk(&mut i.ty, &mut sites);
    }
    for g in &mut program.globals {
        if let Some(t) = &mut g.ty {
            walk(t, &mut sites);
        }
        sites_expr(&mut g.init, &mut HashSet::new(), &mut sites);
    }
    for m in program.protocols.iter_mut().flat_map(|p| &mut p.methods) {
        for t in m.params.iter_mut().chain(std::iter::once(&mut m.ret)) {
            walk(t, &mut sites);
        }
    }
    for m in program.contracts.iter_mut().flat_map(|c| &mut c.members) {
        match &mut m.kind {
            ContractMemberKind::Value { ty, .. } => walk(ty, &mut sites),
            ContractMemberKind::Fn { params, ret, .. } => {
                for t in params.iter_mut().chain(std::iter::once(ret)) {
                    walk(t, &mut sites);
                }
            }
        }
    }
    for b in program.tests.iter_mut().chain(&mut program.benches) {
        sites_block(&mut b.body, &mut HashSet::new(), &mut sites);
    }
}
