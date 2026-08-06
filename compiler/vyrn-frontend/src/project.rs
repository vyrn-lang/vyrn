//! Place projections (RFC-0091 M2) — the `place` member form, and the one
//! mechanism the memory model needed that was genuinely new.
//!
//! A projection yields a *place* inside its receiver instead of returning a
//! value. Rule 3 of RFC-0089 says a return is owned; rule 2 says a borrow may
//! not be returned. A projection is neither: it is **inlined at the access
//! site**, so the borrow it yields lives inside the caller's frame from the
//! first instruction to the last. Rule 2 holds by construction, and no analysis
//! has to prove it.
//!
//! ## What this module does
//!
//! [`lookup`] answers "does this receiver type declare a projection under this
//! name". [`inline`] answers "what does the access site become". Every engine
//! calls exactly those two, at the two sites the source can spell: `a[i]`
//! ([`Expr::Call`] named `at`) and `a[i] = v` ([`Stmt::IndexSet`]).
//!
//! ## The seeded row, and what the dogfood proof really deletes
//!
//! RFC-0091 M2 asks for the hardcoded `Array` indexing to be deleted and
//! re-expressed as the seeded `Index` impl. It cannot be deleted in full, and
//! the RFC does not notice why: it was written before RFC-0080/0081 withdrew
//! raw memory. `Array`'s own `at` has nothing to write its body *with* — there
//! is no way in Vyrn to say "the element at offset i of my buffer".
//!
//! So one primitive stays, under a name no source can spell: [`ELEM`]
//! (`@slot`). It is the addressing floor, exactly as `malloc`/`free`/`memcpy`
//! are the allocation floor RFC-0091 deliberately leaves closed. What the proof
//! does delete is the **dispatch**: `a[i]` no longer means "the compiler knows
//! about arrays". It means "look up a projection for this receiver's type", and
//! `Array` gets there through [`SEEDED`] like anything else. A user container
//! whose `place at` yields `self.data[i]` inlines to the same `@slot` through
//! one more level of the same machinery.

use crate::ast::{Block, Expr, Function, ImplBlock, LambdaBody, Program, Stmt, Type};
use std::collections::HashMap;

/// The element-place primitive: `@slot(container, index)`. Unspellable (no
/// source token lexes to it), and the only indexing the backends still know
/// about by name. `at` — what `a[i]` parses to — is now the *dispatch* site.
pub const ELEM: &str = "@slot";

/// One access site's lowering: statements to run first, then the place.
#[derive(Debug, Clone)]
pub struct Projection {
    /// The projection body's statements before the `yield`, with the receiver
    /// and arguments substituted. Run in the caller's frame, in order.
    pub prologue: Vec<Stmt>,
    /// The yielded place, substituted. A read loads from it; a store writes to
    /// it. It is always a place the backends already address: a variable, a
    /// field of one, or [`ELEM`].
    pub place: Expr,
}

impl Projection {
    /// Is this inline the identity — an empty prologue yielding [`ELEM`] of the
    /// very expressions the access site was written with?
    ///
    /// The seeded row always is, and an engine that sees `true` lowers the
    /// ORIGINAL nodes rather than these substituted copies. That is what makes
    /// "a builtin container costs nothing" a fact about the code rather than a
    /// measurement: the identical path is taken, not merely reached.
    ///
    /// It also keeps the node ADDRESSES. This compiler keys two side tables by
    /// them — the elided `get`/`set` generation checks and the lambda
    /// monomorphization keys — because Phase 4a found a `(line, name)` key
    /// cannot identify a statement. A clone has an address of its own, so an
    /// inlined body loses whatever was recorded against the original. Both
    /// misses are conservative today (one extra check, one duplicated
    /// instance), so this is a cost, not a bug; a projection body carrying
    /// either shape is worth measuring before it ships.
    pub fn is_identity(&self, recv: &Expr, args: &[Expr]) -> bool {
        self.prologue.is_empty()
            && match &self.place {
                Expr::Call { name, args: pa, .. } if name == ELEM && pa.len() == args.len() + 1 => {
                    pa[0] == *recv && pa[1..] == *args
                }
                _ => false,
            }
    }
}

/// The seeded `impl Index for <builtin container>` — what a builtin container's
/// `place at` would say if it could be written:
///
/// ```text
/// impl<T> Index for Array<T> {
///     place at(read self, i: Int64) -> T      { yield @slot(self, i) }
///     place atSet(modify self, i: Int64) -> T { yield @slot(self, i) }
/// }
/// ```
///
/// Built as AST rather than parsed from source, because `@slot` is deliberately
/// unlexable — the lexer rejects `@`, and that is what makes the primitive
/// unspellable in a user's program.
///
/// One row serves every builtin container. The body names no type, so
/// `Array<T>`, `SmallArray<T, N>`, `Array<T, N>`, `String` and `Map<String, V>`
/// all project the same way and each backend types [`ELEM`] for itself. The
/// declared return type is therefore inert; it is spelled `Unit` and never read.
fn seeded_rows() -> Vec<Function> {
    use crate::ast::{Capability, Param};
    ["at", "atSet"]
        .into_iter()
        .map(|name| Function {
            name: name.to_string(),
            exported: false,
            module: None,
            doc: None,
            type_params: Vec::new(),
            type_bounds: Default::default(),
            params: vec![
                Param {
                    name: "self".to_string(),
                    capability: if name == "at" {
                        Capability::Read
                    } else {
                        Capability::Modify
                    },
                    ty: Type::Unit,
                },
                Param {
                    name: "i".to_string(),
                    capability: Capability::Read,
                    ty: Type::Int,
                },
            ],
            ret: Type::Unit,
            body: Block {
                stmts: vec![Stmt::Return {
                    value: Some(Expr::Call {
                        name: ELEM.to_string(),
                        args: vec![
                            Expr::Var {
                                name: "self".to_string(),
                                line: 0,
                            },
                            Expr::Var {
                                name: "i".to_string(),
                                line: 0,
                            },
                        ],
                        line: 0,
                    }),
                    line: 0,
                }],
            },
            line: 0,
            is_extern: false,
            is_export_extern: false,
            is_gen: false,
            is_mut: false,
        })
        .collect()
}

/// Does `ty` index through the seeded row rather than through a user `impl`?
///
/// `pub` because it is the early-out on the hottest path in the checker: every
/// `a[i]` in the program asks whether its receiver projects, and for the
/// overwhelming majority the answer is "it is an Array" — which must cost one
/// pattern match, not a type resolution.
pub fn is_builtin_container(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Array(_)
            | Type::ArrayN(..)
            | Type::SmallArray(..)
            | Type::Str
            | Type::Map(..)
            | Type::Err
    )
}

/// The `place` member named `method` for a receiver of type `ty`, if any.
///
/// A user `impl` wins over the seeded row: a type key can only be one or the
/// other, since a program may not declare an impl for a builtin container.
pub fn lookup<'a>(program: &'a Program, ty: &Type, method: &str) -> Option<&'a Function> {
    lookup_in(&program.impls, ty, method)
}

/// [`lookup`], over an impl list rather than a whole program — the shape the
/// engines hold.
pub fn lookup_in<'a>(impls: &'a [ImplBlock], ty: &Type, method: &str) -> Option<&'a Function> {
    if is_builtin_container(ty) {
        return None;
    }
    lookup_by_key(impls, &crate::types::type_key(ty)?, method)
}

/// [`lookup_in`], by type key rather than by type. The interpreter reaches this
/// one: a record value carries the name `coerce` stamped on it, which is the
/// key, where its static type may only be the anonymous record it aliases.
pub fn lookup_by_key<'a>(
    impls: &'a [ImplBlock],
    key: &str,
    method: &str,
) -> Option<&'a Function> {
    lookup_impl_by_key(impls, key, method).map(|(_, f)| f)
}

/// [`lookup_impl_by_key`], by type. The builtin containers short-circuit before
/// the scan: every `a[i]` in the program reaches this, and a builtin container
/// can never have a user `impl`, so the scan would always come back empty.
pub fn lookup_impl<'a>(
    impls: &'a [ImplBlock],
    ty: &Type,
    method: &str,
) -> Option<(&'a ImplBlock, &'a Function)> {
    if is_builtin_container(ty) {
        return None;
    }
    lookup_impl_by_key(impls, &crate::types::type_key(ty)?, method)
}

/// The impl and the `place` member named `method` for type key `key`.
pub fn lookup_impl_by_key<'a>(
    impls: &'a [ImplBlock],
    key: &str,
    method: &str,
) -> Option<(&'a ImplBlock, &'a Function)> {
    for imp in impls {
        if imp.places.is_empty() {
            continue;
        }
        if crate::types::type_key(&imp.ty).as_deref() != Some(key) {
            continue;
        }
        if let Some(f) = imp.places.iter().find(|f| f.name == method) {
            return Some((imp, f));
        }
    }
    None
}

/// The seeded projection named `method`, for a builtin container.
pub fn seeded(method: &str) -> Option<&'static Function> {
    use std::sync::OnceLock;
    static ROWS: OnceLock<Vec<Function>> = OnceLock::new();
    let rows = ROWS.get_or_init(seeded_rows);
    rows.iter().find(|f| f.name == method)
}

/// The projection an access site resolves to: a user `impl`'s, or the seeded
/// row for a builtin container. `None` means the receiver cannot be indexed,
/// and the caller keeps its own diagnostic.
pub fn resolve<'a>(impls: &'a [ImplBlock], ty: &Type, method: &str) -> Option<&'a Function> {
    if is_builtin_container(ty) {
        return seeded(method);
    }
    lookup_in(impls, ty, method)
}

/// The projection an access site lowers through, given whatever static type the
/// engine could work out for the receiver.
///
/// A receiver whose type the engine cannot name takes the seeded row, which is
/// what every builtin container takes: it yields [`ELEM`] and the engine's own
/// element lowering answers from there. That is also the pre-RFC-0091 behaviour
/// for such a receiver, so nothing regressed on the way in.
pub fn for_site<'a>(
    impls: &'a [ImplBlock],
    recv: Option<&Type>,
    method: &str,
) -> Option<&'a Function> {
    recv.and_then(|t| lookup_in(impls, t, method))
        .or_else(|| seeded(method))
}

/// Inline `f` at an access site whose receiver is `recv` and whose arguments
/// are `args`.
///
/// An argument used exactly once is substituted in place, which is what keeps
/// the seeded row's lowering byte-identical to the hardcoded one it replaces.
/// An argument used zero times or more than once binds a temporary first: zero
/// so its side effects still happen, more than once so they happen once.
pub fn inline(f: &Function, recv: &Expr, args: &[Expr], line: usize) -> Result<Projection, String> {
    if args.len() + 1 != f.params.len() {
        return Err(format!(
            "line {line}: `place {}` takes {} argument(s), got {}",
            f.name,
            f.params.len() - 1,
            args.len()
        ));
    }
    let mut body = f.body.clone();
    // Rename the body's own bindings out of the caller's namespace. A `let n`
    // inside a projection must not capture, or be captured by, a caller's `n`.
    let mut rename: HashMap<String, String> = HashMap::new();
    collect_bindings(&body, &mut rename);
    if !rename.is_empty() {
        let renames: HashMap<String, Expr> = rename
            .iter()
            .map(|(k, v)| (k.clone(), Expr::Var { name: v.clone(), line }))
            .collect();
        rename_bindings(&mut body, &rename);
        subst_block(&mut body, &renames);
    }

    let mut prologue = Vec::new();
    let mut map: HashMap<String, Expr> = HashMap::new();
    // The receiver is a place, never a value: substituting it directly is the
    // whole point. Hoisting it into a `let` would copy the container.
    map.insert("self".to_string(), recv.clone());
    for (p, a) in f.params[1..].iter().zip(args) {
        let uses = count_uses(&body, &p.name);
        if uses == 1 && !is_under_loop(&body, &p.name) {
            map.insert(p.name.clone(), a.clone());
        } else {
            let tmp = format!("@p.{}", p.name);
            prologue.push(Stmt::Let {
                name: tmp.clone(),
                mutable: false,
                ty: None,
                value: a.clone(),
                line,
            });
            map.insert(p.name.clone(), Expr::Var { name: tmp, line });
        }
    }
    subst_block(&mut body, &map);

    let Some(Stmt::Return { value: Some(place), .. }) = body.stmts.last() else {
        return Err(format!(
            "line {line}: `place {}` has no `yield` — a projection ends by \
             yielding the place it names",
            f.name
        ));
    };
    let place = place.clone();
    body.stmts.pop();
    prologue.extend(body.stmts);
    Ok(Projection { prologue, place })
}

// ---------------------------------------------------------------------------
// Substitution
// ---------------------------------------------------------------------------

/// Every binding a projection body introduces, mapped to an unspellable name.
fn collect_bindings(b: &Block, out: &mut HashMap<String, String>) {
    for s in &b.stmts {
        match s {
            Stmt::Let { name, .. } => {
                out.insert(name.clone(), format!("@b.{name}"));
            }
            Stmt::If {
                then_block,
                else_block,
                ..
            }
            | Stmt::IfLet {
                then_block,
                else_block,
                ..
            } => {
                collect_bindings(then_block, out);
                if let Some(e) = else_block {
                    collect_bindings(e, out);
                }
            }
            Stmt::While { body, .. } | Stmt::Region { body, .. } => collect_bindings(body, out),
            Stmt::ForIn { var, body, .. } => {
                out.insert(var.clone(), format!("@b.{var}"));
                collect_bindings(body, out);
            }
            _ => {}
        }
    }
}

/// Rewrite the *declaration* side of each binding through `map`.
fn rename_bindings(b: &mut Block, map: &HashMap<String, String>) {
    for s in &mut b.stmts {
        match s {
            Stmt::Let { name, .. } => {
                if let Some(n) = map.get(name) {
                    *name = n.clone();
                }
            }
            Stmt::If {
                then_block,
                else_block,
                ..
            }
            | Stmt::IfLet {
                then_block,
                else_block,
                ..
            } => {
                rename_bindings(then_block, map);
                if let Some(e) = else_block {
                    rename_bindings(e, map);
                }
            }
            Stmt::While { body, .. } | Stmt::Region { body, .. } => rename_bindings(body, map),
            Stmt::ForIn { var, body, .. } => {
                if let Some(n) = map.get(var) {
                    *var = n.clone();
                }
                rename_bindings(body, map);
            }
            _ => {}
        }
    }
}

/// How many times `name` is read in `b`.
fn count_uses(b: &Block, name: &str) -> usize {
    let mut n = 0;
    let mut probe = b.clone();
    let map: HashMap<String, Expr> = HashMap::new();
    count_block(&mut probe, name, &mut n, &map);
    n
}

fn count_block(b: &mut Block, name: &str, n: &mut usize, _m: &HashMap<String, Expr>) {
    let mut counter = |e: &mut Expr| {
        if matches!(e, Expr::Var { name: v, .. } if v == name) {
            *n += 1;
        }
    };
    walk_block(b, &mut counter);
}

/// Is `name` read inside a loop body? Substituting there would re-evaluate the
/// argument once per turn, so such a parameter always binds a temporary.
fn is_under_loop(b: &Block, name: &str) -> bool {
    fn go(b: &Block, name: &str, in_loop: bool) -> bool {
        for s in &b.stmts {
            match s {
                Stmt::While { body, .. } | Stmt::ForIn { body, .. } => {
                    if count_uses(body, name) > 0 || go(body, name, true) {
                        return true;
                    }
                }
                Stmt::If {
                    then_block,
                    else_block,
                    ..
                }
                | Stmt::IfLet {
                    then_block,
                    else_block,
                    ..
                } => {
                    if go(then_block, name, in_loop) {
                        return true;
                    }
                    if let Some(e) = else_block {
                        if go(e, name, in_loop) {
                            return true;
                        }
                    }
                }
                Stmt::Region { body, .. } => {
                    if go(body, name, in_loop) {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }
    go(b, name, false)
}

fn subst_block(b: &mut Block, map: &HashMap<String, Expr>) {
    let mut f = |e: &mut Expr| {
        if let Expr::Var { name, .. } = e {
            if let Some(r) = map.get(name) {
                *e = r.clone();
            }
        }
    };
    walk_block(b, &mut f);
}

/// Apply `f` to every expression node in `b`, innermost-last: `f` sees a node
/// after its children, so a substituted expression is never re-walked.
fn walk_block(b: &mut Block, f: &mut impl FnMut(&mut Expr)) {
    for s in &mut b.stmts {
        walk_stmt(s, f);
    }
}

fn walk_stmt(s: &mut Stmt, f: &mut impl FnMut(&mut Expr)) {
    match s {
        Stmt::Let { value, .. } | Stmt::Assign { value, .. } | Stmt::SetField { value, .. } => {
            walk_expr(value, f)
        }
        Stmt::IndexSet { index, value, .. } => {
            walk_expr(index, f);
            walk_expr(value, f);
        }
        Stmt::Return { value: Some(e), .. } => walk_expr(e, f),
        Stmt::Return { value: None, .. } | Stmt::Break { .. } | Stmt::Continue { .. } => {}
        Stmt::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            walk_expr(cond, f);
            walk_block(then_block, f);
            if let Some(e) = else_block {
                walk_block(e, f);
            }
        }
        Stmt::IfLet {
            scrutinee,
            then_block,
            else_block,
            ..
        } => {
            walk_expr(scrutinee, f);
            walk_block(then_block, f);
            if let Some(e) = else_block {
                walk_block(e, f);
            }
        }
        Stmt::While { cond, body, .. } => {
            walk_expr(cond, f);
            walk_block(body, f);
        }
        Stmt::ForIn { iter, body, .. } => {
            walk_expr(iter, f);
            walk_block(body, f);
        }
        Stmt::Drop { .. } => {}
        Stmt::Expr(e) => walk_expr(e, f),
        Stmt::Region { body, .. } => walk_block(body, f),
    }
}

fn walk_expr(e: &mut Expr, f: &mut impl FnMut(&mut Expr)) {
    match e {
        Expr::Int(_)
        | Expr::Byte(_)
        | Expr::Float(_)
        | Expr::Bool(_)
        | Expr::Str(_)
        | Expr::Var { .. } => {}
        Expr::Unary { expr, .. } | Expr::Try { expr, .. } | Expr::Field { expr, .. } => {
            walk_expr(expr, f)
        }
        Expr::Binary { lhs, rhs, .. } => {
            walk_expr(lhs, f);
            walk_expr(rhs, f);
        }
        Expr::Call { args, .. } | Expr::Spawn { args, .. } | Expr::TryConstruct { args, .. } => {
            for a in args {
                walk_expr(a, f);
            }
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            walk_expr(scrutinee, f);
            for a in arms {
                walk_expr(&mut a.body, f);
            }
        }
        Expr::IfExpr {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            walk_expr(cond, f);
            walk_expr(then_branch, f);
            if let Some(e) = else_branch {
                walk_expr(e, f);
            }
        }
        Expr::StructLit { fields, .. } => {
            for (_, v) in fields {
                walk_expr(v, f);
            }
        }
        Expr::ArrayLit { elems, .. } => {
            for v in elems {
                walk_expr(v, f);
            }
        }
        Expr::MapLit { entries, .. } => {
            for (k, v) in entries {
                walk_expr(k, f);
                walk_expr(v, f);
            }
        }
        Expr::Lambda { body, .. } => match body {
            LambdaBody::Expr(e) => walk_expr(e, f),
            LambdaBody::Block(b) => walk_block(b, f),
        },
    }
    f(e);
}

/// Is `e` a place — something with an address the access site can read from or
/// write to, rather than a value it would have to copy?
///
/// A variable, a field of a place, and an element of a place. Nothing else: a
/// call result, a literal and an arithmetic result are all values.
pub fn is_place(e: &Expr) -> bool {
    match e {
        Expr::Var { .. } => true,
        Expr::Field { expr, .. } => is_place(expr),
        Expr::Call { name, args, .. } if (name == "at" || name == ELEM) && args.len() == 2 => {
            is_place(&args[0])
        }
        _ => false,
    }
}

/// The variable a place is rooted at, e.g. `self` for `self.data[i]`.
pub fn place_root(e: &Expr) -> Option<String> {
    match e {
        Expr::Var { name, .. } => Some(name.clone()),
        Expr::Field { expr, .. } => place_root(expr),
        Expr::Call { name, args, .. } if (name == "at" || name == ELEM) && args.len() == 2 => {
            place_root(&args[0])
        }
        _ => None,
    }
}

/// Does `b` use `?` anywhere? A projection may not: `?` propagates by
/// returning, and an inlined projection has no frame of its own to return from.
pub fn has_try(b: &Block) -> bool {
    let mut found = false;
    let mut probe = b.clone();
    walk_block(&mut probe, &mut |e: &mut Expr| {
        if matches!(e, Expr::Try { .. }) {
            found = true;
        }
    });
    found
}

/// Does this program declare any `place` member at all?
///
/// An engine that must WORK to name a receiver's type asks this first. Where
/// the answer is no, every access site takes the seeded row and the work is
/// wasted — and in the direct backend the type probe is `&mut self`, so the
/// wasted work was also visible in the emitted bytes.
pub fn any(impls: &[ImplBlock]) -> bool {
    impls.iter().any(|i| !i.places.is_empty())
}

/// The `place` members of every impl in `p`, for the checker and the LSP.
pub fn all(p: &Program) -> impl Iterator<Item = (&ImplBlock, &Function)> {
    p.impls
        .iter()
        .flat_map(|i| i.places.iter().map(move |f| (i, f)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Program {
        crate::parser::parse(crate::lexer::lex(src).unwrap()).unwrap()
    }

    #[test]
    fn a_projection_parses_and_is_not_a_function() {
        let p = parse(
            "type Ring = { data: Array<Int64> }\n\
             impl Index for Ring {\n\
                 place at(read self, i: Int64) -> Int64 { yield self.data[i] }\n\
             }\n\
             fn main() { print(1) }\n",
        );
        assert_eq!(p.impls[0].places.len(), 1);
        assert_eq!(p.impls[0].methods.len(), 0);
        // A projection is never callable: it does not reach `functions`.
        assert!(!p.functions.iter().any(|f| f.name.contains("__at")));
    }

    #[test]
    fn a_single_use_argument_substitutes_in_place() {
        let p = parse(
            "type Ring = { data: Array<Int64> }\n\
             impl Index for Ring {\n\
                 place at(read self, i: Int64) -> Int64 { yield self.data[i] }\n\
             }\n\
             fn main() { print(1) }\n",
        );
        let f = lookup(&p, &Type::Named("Ring".into()), "at").unwrap();
        let recv = Expr::Var {
            name: "r".into(),
            line: 1,
        };
        let idx = Expr::Binary {
            op: crate::ast::BinOp::Add,
            lhs: Box::new(Expr::Var {
                name: "k".into(),
                line: 1,
            }),
            rhs: Box::new(Expr::Int(1)),
            line: 1,
        };
        let pr = inline(f, &recv, std::slice::from_ref(&idx), 1).unwrap();
        assert!(pr.prologue.is_empty(), "no temp for a single use");
        let Expr::Call { name, args, .. } = &pr.place else {
            panic!("expected the yielded place to stay a call")
        };
        assert_eq!(name, "at");
        assert!(matches!(&args[0], Expr::Field { field, .. } if field == "data"));
        assert_eq!(args[1], idx, "the index substituted in place");
    }

    #[test]
    fn the_seeded_row_yields_the_primitive() {
        let f = seeded("at").unwrap();
        let pr = inline(
            f,
            &Expr::Var {
                name: "a".into(),
                line: 3,
            },
            &[Expr::Int(2)],
            3,
        )
        .unwrap();
        assert!(pr.prologue.is_empty());
        let Expr::Call { name, args, .. } = &pr.place else {
            panic!("expected @slot")
        };
        assert_eq!(name, ELEM);
        assert_eq!(args.len(), 2);
    }

    #[test]
    fn a_prologue_binding_cannot_capture_a_caller_name() {
        let p = parse(
            "type Ring = { data: Array<Int64> }\n\
             impl Index for Ring {\n\
                 place at(read self, i: Int64) -> Int64 {\n\
                     let j = i * 2\n\
                     yield self.data[j]\n\
                 }\n\
             }\n\
             fn main() { print(1) }\n",
        );
        let f = lookup(&p, &Type::Named("Ring".into()), "at").unwrap();
        let pr = inline(
            f,
            &Expr::Var {
                name: "r".into(),
                line: 1,
            },
            &[Expr::Int(3)],
            1,
        )
        .unwrap();
        assert_eq!(pr.prologue.len(), 1);
        assert!(matches!(&pr.prologue[0], Stmt::Let { name, .. } if name == "@b.j"));
    }
}
