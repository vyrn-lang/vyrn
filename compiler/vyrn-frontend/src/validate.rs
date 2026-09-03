//! Where a `where` predicate runs — RFC-0003, stated once for all three
//! engines (RFC-0125 §3 M6, the third judgment).
//!
//! RFC-0003 gives a named type a predicate and RFC-0125 §2.3 says the type is
//! then the proof: a value of it exists only if the predicate held. Until the
//! kernel makes that a judgment, every engine still checks at every boundary,
//! and this file is the one statement of WHICH boundary and WHICH declaration.
//!
//! It is in `vyrn-frontend` for the reason [`crate::trap`] is (RFC-0101 §6.4):
//! `vyrn-codegen` depends on this crate, so the two compiled backends could
//! share a decision between themselves and the interpreter — which lives in the
//! crate they both depend on — could not read it. The census of §3 M6 counted
//! what that cost: `validation_required` in `vyrn-codegen` for the two
//! emitters, and the same question asked three more times in `interp.rs`.
//!
//! Three functions. [`of`] and [`required`] answer the same question at two
//! grains — the interpreter has a value and a target type, the emitters have a
//! pair of types — so the second is built on the first. [`is_cross_field`] is
//! the fact both the BINDING and the WORDING of a predicate follow, asked here
//! so the two cannot disagree.

use std::collections::HashMap;

use crate::ast::{Type, TypeDecl};

/// The declaration whose predicate a value ENTERING a named type must satisfy,
/// or `None` when nothing is checked there.
///
/// One question: does the declaration this name resolved to carry a `where`?
/// An unresolved name is not an error — a generic parameter reaches this with
/// its own spelling and validates nothing.
///
/// The LOOKUP is the caller's and the RULE is here, because the engines hold
/// their declarations differently: the two emitters key a `HashMap` by `String`
/// and own the values, the interpreter keys by `&str` and borrows them. Taking
/// the map would have made this function one engine's map shape, which is how a
/// rule ends up with two spellings.
pub fn of<T: std::borrow::Borrow<TypeDecl>>(found: Option<T>) -> Option<T> {
    found.filter(|d| d.borrow().predicate.is_some())
}

/// [`of`] at a boundary between two known types — the form both compiled
/// backends ask, where the source type is in hand.
///
/// It carries ONE exemption the value form cannot: the exactly-same named type
/// is not a boundary crossing, because it was checked when it was built, so
/// re-running the predicate would be work that cannot fail. The interpreter has
/// no source type at its `coerce` and therefore takes that work; the verdicts
/// are the same either way, which is why one rule can have two entry points and
/// still be one rule.
///
/// A second exemption is deliberately NOT here, because it needs the expression
/// rather than the two types: [`crate::finite::string_flow_proven`], RFC-0020's
/// containment proof. Both backends call that function themselves on the same
/// AST — the consteval precedent — so it is single-sourced too, just one layer
/// out.
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

/// The width and signedness of an integer type, or `None` for a type that is
/// not one. `Int` is `Int64` (RFC-0002: the unsized names are gone).
fn width(t: &Type) -> Option<(u8, bool)> {
    match t {
        Type::Int => Some((64, true)),
        Type::IntN { bits, signed } => Some((*bits, *signed)),
        _ => None,
    }
}

/// Whether a value crossing from `from` into `to` is RE-READ: the census's
/// `int-narrowing` and `float-to-int` rows (RFC-0125 §3 M6).
///
/// These two rows answer rather than refusing — `UInt8(300)` is 44 — so the
/// crossing has no predicate and no trap. What it has is a rule about the
/// bits, and the rule is here for the reason the rest of this file is: the
/// three engines each write their own instructions for it, and they must agree
/// about WHICH crossings do it. A crossing that changes the width or the
/// signedness re-reads the low bits and the sign; the same pair does not, and
/// that is [`required`]'s exemption at the other rows.
pub fn narrows(from: &Type, to: &Type) -> bool {
    let Some(t) = width(to) else { return false };
    match from {
        // Truncated toward zero, then re-read at the target's width.
        Type::Float | Type::Float32 => true,
        _ => width(from).is_some_and(|f| f != t),
    }
}

/// Whether `decl`'s predicate is the CROSS-FIELD form — RFC-0003.
///
/// A record base binds every field name; every other base binds `value`. The
/// wording follows the same fact ([`crate::trap::validation_of`]), so the two
/// are asked here rather than spelled at each site that runs a predicate.
pub fn is_cross_field(decl: &TypeDecl) -> bool {
    matches!(decl.base, Type::Record(_))
}
