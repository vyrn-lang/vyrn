//! Finite string types and interpolation containment.
//!
//! A validated `String` type whose predicate is a pure `value =~ "..."`
//! conjunction compiles to a DFA ([`crate::regex`]); when its language is
//! finite, the type is a finite string type, and the checker gains two powers:
//!
//! 1. Interpolation typing. `"nav.\{s}.label"`, with every hole a finite
//!    string type or a constant, denotes a finite regular language. Coercing it
//!    into a validated string type checks containment by DFA product: a proof
//!    emits no runtime check, and a failure is a compile error carrying the
//!    shortest witness.
//! 2. Finite variable flow. A value of finite string type `S` flowing into `T`
//!    skips the runtime check when `L(S)` is inside `L(T)`; otherwise the check
//!    stays, never an error.
//!
//! The parser desugars `"a\{e}b"` into a `@concat` chain over `Expr::Str`
//! parts and `@str(e)` holes, which [`flatten_template`] recognises;
//! `x.toString()` is the same `@str(x)`. Containment applies only when both
//! sides are pure-regex types; any other predicate keeps its runtime
//! validation. [`string_flow_proven`] is the oracle each backend runs on the
//! same AST, so they agree without sharing analysis results.

use std::collections::HashMap;

use crate::ast::{BinOp, Expr, Type, TypeDecl};
use crate::consteval::{self, ConstVal};
use crate::regex::{self, ConcatPiece, Dfa};
use crate::types::Decls;

/// Returns the DFA of a validated `String` type whose predicate is a pure
/// conjunction of `value =~ "lit"` clauses: the intersection of the clause
/// languages. `None` for any other type or predicate.
fn regex_dfa_of_type(decl: &TypeDecl) -> Option<Dfa> {
    let pats = patterns_of(decl)?;
    let mut dfa = regex::compile(&pats[0]).ok()?;
    for p in &pats[1..] {
        let next = regex::compile(p).ok()?;
        dfa = regex::intersect(&dfa, &next);
    }
    Some(dfa)
}

/// Returns the `value =~ "literal"` clauses of a validated `String` type, in
/// order: the input of [`regex_dfa_of_type`], and a cheap key for its language
/// that the enumeration memo uses.
fn patterns_of(decl: &TypeDecl) -> Option<Vec<String>> {
    if decl.base != Type::Str {
        return None;
    }
    let pred = decl.predicate.as_ref()?;
    let mut pats: Vec<String> = Vec::new();
    collect_match_clauses(pred, &mut pats)?;
    if pats.is_empty() {
        return None;
    }
    Some(pats)
}

/// Gathers the `value =~ "literal"` clauses of a pure conjunction of them;
/// `None` on any other shape, so a mixed predicate is not a regex language.
fn collect_match_clauses(pred: &Expr, out: &mut Vec<String>) -> Option<()> {
    match pred {
        Expr::Binary {
            op: BinOp::And,
            lhs,
            rhs,
            ..
        } => {
            collect_match_clauses(lhs, out)?;
            collect_match_clauses(rhs, out)?;
            Some(())
        }
        Expr::Binary {
            op: BinOp::Match,
            lhs,
            rhs,
            ..
        } => match (&**lhs, &**rhs) {
            (Expr::Var { name, .. }, Expr::Str(pat, _)) if name == "value" => {
                out.push(pat.clone());
                Some(())
            }
            _ => None,
        },
        _ => None,
    }
}

/// Returns the declaration of `ty` when it is a named validated type.
fn string_type_decl<'a>(ty: &Type, types: &'a dyn Decls) -> Option<&'a TypeDecl> {
    match ty {
        Type::Named(n) => types.decl(n).filter(|d| d.predicate.is_some()),
        _ => None,
    }
}

/// One piece of a flattened interpolation: a literal part, or the expression of
/// an `@str(e)` hole.
enum Piece<'a> {
    Lit(String),
    Hole(&'a Expr),
}

/// Flattens a desugared interpolation chain (`@concat` and `@str` over
/// `Expr::Str` leaves, from `parser::template`) left to right into literal
/// parts and holes. `None` when `expr` is not such a chain. A result with no
/// hole is a plain literal, which ordinary coercion handles.
fn flatten_template(expr: &Expr) -> Option<Vec<Piece<'_>>> {
    fn walk<'a>(e: &'a Expr, out: &mut Vec<Piece<'a>>) -> Option<()> {
        match e {
            Expr::Str(s, _) => {
                out.push(Piece::Lit(s.clone()));
                Some(())
            }
            Expr::Call { name, args, .. } if name == "@str" && args.len() == 1 => {
                out.push(Piece::Hole(&args[0]));
                Some(())
            }
            Expr::Call { name, args, .. } if name == "@concat" && args.len() == 2 => {
                walk(&args[0], out)?;
                walk(&args[1], out)?;
                Some(())
            }
            _ => None,
        }
    }
    let mut out = Vec::new();
    walk(expr, &mut out)?;
    Some(out)
}

/// Returns whether `pieces` hold a hole, so the chain is an interpolation.
fn has_hole(pieces: &[Piece]) -> bool {
    pieces.iter().any(|p| matches!(p, Piece::Hole(_)))
}

/// Builds the concatenation language of an interpolation's `pieces`. `None` if
/// a hole is neither a constant string nor a finite string type, and the
/// runtime validation stands. `resolve` gives a hole's type: the checker's
/// inferer, or a backend's scope types.
fn template_language(
    pieces: &[Piece],
    types: &dyn Decls,
    resolve: &dyn Fn(&Expr) -> Option<Type>,
) -> Option<Dfa> {
    enum Owned {
        Lit(Vec<u8>),
        Dfa(Dfa),
    }
    let mut owned: Vec<Owned> = Vec::new();
    for piece in pieces {
        match piece {
            Piece::Lit(s) => owned.push(Owned::Lit(s.clone().into_bytes())),
            Piece::Hole(e) => {
                // A hole that folds to a constant string is a literal.
                if let Some(ConstVal::Str(s)) = consteval::eval(e, &HashMap::new()) {
                    owned.push(Owned::Lit(s.into_bytes()));
                    continue;
                }
                // Otherwise the hole must be a finite string type.
                let ty = resolve(e)?;
                let decl = string_type_decl(&ty, types)?;
                let dfa = regex_dfa_of_type(decl)?;
                if !dfa.is_finite() {
                    return None;
                }
                owned.push(Owned::Dfa(dfa));
            }
        }
    }
    let refs: Vec<ConcatPiece> = owned
        .iter()
        .map(|o| match o {
            Owned::Lit(b) => ConcatPiece::Lit(b),
            Owned::Dfa(d) => ConcatPiece::Dfa(d),
        })
        .collect();
    Some(regex::concat_language(&refs))
}

/// The outcome of proving a string coercion.
pub enum Proof {
    /// Not a finite-string containment case: runtime behaviour is unchanged.
    NotApplicable,
    /// Proven contained: a backend may skip the runtime check.
    Proven,
    /// The interpolation can produce `witness`, outside the target language: a
    /// compile error.
    Witness(String),
}

/// Proves or refutes that `expr` flowing into the validated string type `to`
/// is contained. `resolve` types the holes and, in the variable case, `expr`.
/// The checker's error path and the backends' skip path share it.
pub fn prove_string_flow(
    expr: &Expr,
    to: &Type,
    types: &dyn Decls,
    resolve: &dyn Fn(&Expr) -> Option<Type>,
) -> Proof {
    // The target must be a pure-regex validated string type.
    let Some(tdecl) = string_type_decl(to, types) else {
        return Proof::NotApplicable;
    };
    let Some(target) = regex_dfa_of_type(tdecl) else {
        return Proof::NotApplicable;
    };

    // An interpolation's language is exactly what it can produce, so
    // non-containment is an error with a witness.
    if let Some(pieces) = flatten_template(expr) {
        if has_hole(&pieces) {
            match template_language(&pieces, types, resolve) {
                Some(l) => {
                    return match regex::contains(&target, &l) {
                        Ok(()) => Proof::Proven,
                        Err(witness) => Proof::Witness(witness),
                    }
                }
                // A hole that is not finite keeps the runtime validation.
                None => return Proof::NotApplicable,
            }
        }
    }

    // A value of finite string type flowing into `T` skips the check when its
    // language is contained; otherwise the check stays.
    if let Some(sty) = resolve(expr) {
        if let Some(sdecl) = string_type_decl(&sty, types) {
            if let Some(sdfa) = regex_dfa_of_type(sdecl) {
                if sdfa.is_finite() && regex::contains(&target, &sdfa).is_ok() {
                    return Proof::Proven;
                }
            }
        }
    }
    Proof::NotApplicable
}

/// Returns whether the flow is proven contained, so a backend may skip the
/// runtime validation. A `Witness`, which a checked program cannot have, keeps
/// the check.
pub fn string_flow_proven(
    expr: &Expr,
    to: &Type,
    types: &dyn Decls,
    resolve: &dyn Fn(&Expr) -> Option<Type>,
) -> bool {
    matches!(prove_string_flow(expr, to, types, resolve), Proof::Proven)
}

/// Returns the language of a finite string type for LSP completion; `None`
/// if it is not one or has more than `cap` members.
pub fn enumerate_type(decl: &TypeDecl, cap: usize) -> Option<Vec<String>> {
    memo_enum(decl, cap, false)
}

/// Returns the alphabet of a sequence validated string type: an infinite
/// pure-regex type of space-separated tokens (`token( token)*`).
/// The alphabet is every member without a space, read from the DFA the
/// compiler checks against. `None` for a finite type (see
/// [`enumerate_type`]), a type that is not pure-regex, or more than `cap`
/// members.
pub fn enumerate_alphabet(decl: &TypeDecl, cap: usize) -> Option<Vec<String>> {
    memo_enum(decl, cap, true)
}

/// Both enumerations, memoized on the clause patterns, which alone determine
/// the language. The indexer walks every type of the linked program on every
/// analysis, and the uncached walks measured ~177 ms per keystroke.
fn memo_enum(decl: &TypeDecl, cap: usize, alphabet: bool) -> Option<Vec<String>> {
    let pats = patterns_of(decl)?;
    // ponytail: unbounded, like the DFA memo, but keyed by patterns in the
    // analyzed program.
    thread_local! {
        static MEMO: std::cell::RefCell<
            std::collections::HashMap<(Vec<String>, usize, bool), Option<Vec<String>>>,
        > = std::cell::RefCell::new(std::collections::HashMap::new());
    }
    let key = (pats, cap, alphabet);
    if let Some(hit) = MEMO.with(|m| m.borrow().get(&key).cloned()) {
        return hit;
    }
    let out = enumerate_uncached(decl, cap, alphabet);
    MEMO.with(|m| m.borrow_mut().insert(key, out.clone()));
    out
}

fn enumerate_uncached(decl: &TypeDecl, cap: usize, alphabet: bool) -> Option<Vec<String>> {
    let dfa = regex_dfa_of_type(decl)?;
    if !alphabet {
        return dfa.enumerate(cap);
    }
    // A finite type's members are its completions (`enumerate_type`); only an
    // infinite type can be a sequence.
    if dfa.is_finite() {
        return None;
    }
    dfa.enumerate_without(b' ', cap)
}
