//! Where a `where` predicate runs: the one statement of which
//! boundary checks which declaration, for every backend.
//!
//! [`of`] and [`required`] answer the same question at two grains, a found
//! declaration and a pair of types. [`is_cross_field`] decides both how a
//! predicate binds and how its failure reads. The crossings that re-read bits
//! rather than refuse follow: [`narrows`] says which crossings do, [`wrap`]
//! what the value reads as, and [`width`] which types are integers. Each
//! backend writes its own instructions, but all agree here on which crossing
//! does what. It lives in `vyrn-frontend` so every backend can read it.

use std::collections::HashMap;

use crate::ast::{Expr, Type, TypeDecl};
use crate::consteval::{self, ConstVal};

/// Returns the declaration whose predicate a value entering a named type must
/// satisfy, or `None` when nothing is checked. An unresolved name (a generic
/// parameter) validates nothing. The caller does the lookup, so the rule
/// takes no particular map shape.
pub fn of<T: std::borrow::Borrow<TypeDecl>>(found: Option<T>) -> Option<T> {
    found.filter(|d| d.borrow().predicate.is_some())
}

/// [`of`] at a boundary between two known types, as the backends ask it. The
/// exactly-same named type is no crossing: it was checked when it was built.
/// [`proven`] is the other exemption, which needs the expression.
pub fn required<'t>(
    from: &Type,
    to: &Type,
    types: &'t HashMap<String, TypeDecl>,
) -> Option<&'t TypeDecl> {
    let Type::Named(n) = to else { return None };
    if from == to {
        return None;
    }
    of(types.get(n))
}

/// The checker's verdict on a constant crossing into `decl`:
/// `Some(true)` where the predicate holds, `Some(false)` where the checker
/// refuses the crossing, and `None` where the value or the predicate is not
/// a constant, which leaves the runtime check. A scalar binds `value`; a
/// record literal over a record base binds each field. The scalar's constant
/// is handed back for the refusal's wording.
pub fn constant_verdict(expr: &Expr, decl: &TypeDecl) -> Option<(bool, Option<ConstVal>)> {
    let pred = decl.predicate.as_ref()?;
    let (env, cv) = match (consteval::eval(expr, &HashMap::new()), expr, &decl.base) {
        (Some(cv), ..) => (HashMap::from([("value".to_string(), cv.clone())]), Some(cv)),
        (None, Expr::StructLit { fields, .. }, Type::Record(_)) => {
            let env = fields
                .iter()
                .map(|(f, e)| Some((f.clone(), consteval::eval(e, &HashMap::new())?)))
                .collect::<Option<HashMap<_, _>>>()?;
            (env, None)
        }
        _ => return None,
    };
    consteval::eval(pred, &env)
        .and_then(ConstVal::as_bool)
        .map(|holds| (holds, cv))
}

/// Returns whether the checker proved `e` a value of the validated type `to`,
/// so the crossing runs no check: a constant whose predicate holds
/// ([`constant_verdict`]), or a string flow interpolation containment proves
/// ([`crate::finite::string_flow_proven`]). `resolve` types a name in the
/// caller's scope. Every walk that skips a check asks this.
pub fn proven(
    e: &Expr,
    to: &Type,
    types: &HashMap<String, TypeDecl>,
    resolve: &dyn Fn(&Expr) -> Option<Type>,
) -> bool {
    let Type::Named(n) = to else { return false };
    types
        .get(n)
        .and_then(|d| constant_verdict(e, d))
        .is_some_and(|(holds, _)| holds)
        || crate::finite::string_flow_proven(e, to, types, resolve)
}

/// Returns the width and signedness of an integer type, or `None`. `Int` is
/// `Int64`.
pub fn width(t: &Type) -> Option<(u8, bool)> {
    match t {
        Type::Int => Some((64, true)),
        Type::IntN { bits, signed } => Some((*bits, *signed)),
        _ => None,
    }
}

/// Returns whether a value crossing from `from` into `to` is re-read: the
/// `int-narrowing` and `float-to-int` rows.
///
/// These crossings answer rather than refuse (`UInt8(300)` is 44), so they
/// have no predicate and no trap. A crossing that changes width or signedness
/// re-reads the low bits and the sign; the same pair does not.
pub fn narrows(from: &Type, to: &Type) -> bool {
    let Some(t) = width(to) else { return false };
    match from {
        // Truncated toward zero, then re-read at the target's width.
        Type::Float | Type::Float32 => true,
        _ => width(from).is_some_and(|f| f != t),
    }
}

/// Returns what `v` reads as at `bits` and `signed`: the low bits, with the
/// sign re-read at the new width. [`narrows`] says where it applies.
pub fn wrap(v: i64, bits: u8, signed: bool) -> i64 {
    if bits >= 64 {
        return v;
    }
    let mask = (1i64 << bits) - 1;
    let m = v & mask;
    if signed && (m & (1i64 << (bits - 1))) != 0 {
        m | !mask // sign extension
    } else {
        m
    }
}

/// Returns whether `decl`'s predicate is cross-field: a record base
/// binds every field name, any other base binds `value`. The failure wording
/// ([`crate::trap::validation_of`]) follows the same fact.
pub fn is_cross_field(decl: &TypeDecl) -> bool {
    matches!(decl.base, Type::Record(_))
}
