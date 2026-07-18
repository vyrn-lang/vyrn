//! Finite string types and interpolation containment (RFC-0020 M1).
//!
//! A validated `String` type whose refinement is a **pure** `value =~ "…"`
//! conjunction compiles to a DFA (see [`crate::regex`]). When that DFA denotes a
//! *finite* language it is a **finite string type** — and two checker powers
//! follow (the flagship DX of RFC-0020):
//!
//! 1. **Interpolation typing.** A string interpolation `"nav.\{s}.label"` whose
//!    every hole is a finite string type (or a constant) denotes the finite
//!    regular language `lit0 · H1 · lit1 · …`. Coercing it into a validated
//!    string type `T` checks **L ⊆ T** by DFA product (polynomial — never the
//!    combinatorial union expansion TypeScript pays). Proven ⇒ no runtime check
//!    is emitted (the consteval precedent); not contained ⇒ a compile error
//!    carrying the product automaton's shortest **witness**.
//! 2. **Finite variable flow.** A plain expression of a finite string type `S`
//!    flowing into `T` skips the runtime check when `L(S) ⊆ L(T)`; otherwise the
//!    runtime check stays (a variable might still hold a conforming value — this
//!    is an optimisation, never an error, unlike interpolations whose language is
//!    exactly what they can produce).
//!
//! ## The interpolation AST
//!
//! The parser desugars `"a\{e}b"` into a left-folded `@concat` chain over
//! `Expr::Str` literal parts and `@str(e)` holes (see `parser::template`); there
//! is no first-class interpolation node by the time the checker runs. So we
//! *recognise the desugared chain* here ([`flatten_template`]) rather than hook
//! pre-desugar — both are acceptable per the RFC, and pattern-matching the chain
//! keeps this a pure analysis with zero parser churn. `x.toString()` desugars to
//! the same `@str(x)`, so a bare `"\{x}"` and `x.toString()` are treated
//! identically (correct: both denote the string form of `x`).
//!
//! ## Scope of containment (the pure-regex rule)
//!
//! Containment applies only when BOTH sides are pure-regex validated strings
//! (a `where value =~ "a" && value =~ "b"` conjunction — realised as the DFA
//! intersection). A target whose predicate mixes in a length or other clause is
//! not a regex language here, so such a flow falls back to the ordinary runtime
//! validation, unchanged.
//!
//! ## Where the skip happens
//!
//! [`string_flow_proven`] is the boolean oracle both backends run independently
//! on the same AST (the consteval precedent), so they agree without threading
//! any analysis result. **Codegen** consults it at every value boundary and, on
//! a proof, emits no validation block — the RFC's "no runtime cost". **The
//! interpreter** deliberately leaves its runtime validation in place: on a
//! statically-proven value the predicate holds by construction, so the check is
//! a guaranteed no-op and the interpreter's observable behaviour is byte-
//! identical to codegen's skip. Keeping the interpreter's hot path untouched
//! preserves the sacred `interp == native == wasm` invariant with zero risk;
//! only codegen materially benefits from eliding the check.

use std::collections::HashMap;

use crate::ast::{BinOp, Expr, Type, TypeDecl};
use crate::consteval::{self, ConstVal};
use crate::regex::{self, ConcatPiece, Dfa};

/// The DFA of a validated **String** type whose predicate is a *pure*
/// `value =~ "lit"` conjunction (a single clause, or several joined by `&&`).
/// The conjunction's language is the intersection of the clause languages.
/// Returns `None` when the type is not a `String`-based validated type, has no
/// predicate, or the predicate contains anything other than `value =~ <literal>`
/// clauses (a length or comparison clause ⇒ not a regex language ⇒ runtime
/// fallback).
pub fn regex_dfa_of_type(decl: &TypeDecl) -> Option<Dfa> {
    if decl.base != Type::Str {
        return None;
    }
    let pred = decl.predicate.as_ref()?;
    let mut pats: Vec<String> = Vec::new();
    collect_match_clauses(pred, &mut pats)?;
    if pats.is_empty() {
        return None;
    }
    let mut dfa = regex::compile(&pats[0]).ok()?;
    for p in &pats[1..] {
        let next = regex::compile(p).ok()?;
        dfa = regex::intersect(&dfa, &next);
    }
    Some(dfa)
}

/// Gather every `value =~ "literal"` clause from a predicate that is a pure
/// conjunction of them. Returns `None` (aborting the whole extraction) the moment
/// any other shape is seen, so a mixed predicate is not mistaken for a regex
/// language.
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
            // Exactly `value =~ "pattern-literal"`.
            (Expr::Var { name, .. }, Expr::Str(pat)) if name == "value" => {
                out.push(pat.clone());
                Some(())
            }
            _ => None,
        },
        _ => None,
    }
}

/// Whether `decl` is a **finite** string type: a pure-regex validated `String`
/// whose language is finite.
pub fn is_finite_string_type(decl: &TypeDecl) -> bool {
    regex_dfa_of_type(decl)
        .map(|d| d.is_finite())
        .unwrap_or(false)
}

/// If `ty` is a named validated `String` type, return its declaration.
pub fn string_type_decl<'a>(
    ty: &Type,
    types: &'a HashMap<String, TypeDecl>,
) -> Option<&'a TypeDecl> {
    match ty {
        Type::Named(n) => types.get(n).filter(|d| d.predicate.is_some()),
        _ => None,
    }
}

/// One flattened piece of a desugared interpolation: a literal part, or a hole
/// expression (`@str(e)` → `Hole(e)`).
pub enum Piece<'a> {
    Lit(String),
    Hole(&'a Expr),
}

/// Recognise a desugared interpolation chain (`@concat`/`@str` over `Expr::Str`
/// leaves, as produced by `parser::template`) and flatten it left-to-right into
/// its literal parts and holes. Returns `None` if `expr` is not such a chain.
/// A returned vector contains at least one [`Piece::Hole`] only if the source
/// actually interpolated; callers should treat a hole-free result as "not an
/// interpolation" (a plain string literal is handled by ordinary coercion).
pub fn flatten_template(expr: &Expr) -> Option<Vec<Piece<'_>>> {
    fn walk<'a>(e: &'a Expr, out: &mut Vec<Piece<'a>>) -> Option<()> {
        match e {
            Expr::Str(s) => {
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

/// Whether flattened `pieces` contain at least one hole (i.e. this really is an
/// interpolation, not a bare literal).
pub fn has_hole(pieces: &[Piece]) -> bool {
    pieces.iter().any(|p| matches!(p, Piece::Hole(_)))
}

/// Build the concatenation language of an interpolation's `pieces`, given a way
/// to resolve each non-constant hole's **type**. Returns `None` if any hole is
/// neither a compile-time-constant string nor a finite string type — in which
/// case containment does not apply and the ordinary runtime validation stands.
///
/// `resolve` maps a hole expression to its type (the checker uses its inferer;
/// the backends use their scope's declared types). Owned DFAs are held in `bufs`
/// so the returned language can borrow them.
pub fn template_language(
    pieces: &[Piece],
    types: &HashMap<String, TypeDecl>,
    resolve: &dyn Fn(&Expr) -> Option<Type>,
) -> Option<Dfa> {
    // Materialise each piece as an owned literal or an owned DFA, then borrow.
    enum Owned {
        Lit(Vec<u8>),
        Dfa(Dfa),
    }
    let mut owned: Vec<Owned> = Vec::new();
    for piece in pieces {
        match piece {
            Piece::Lit(s) => owned.push(Owned::Lit(s.clone().into_bytes())),
            Piece::Hole(e) => {
                // A hole that folds to a constant string is itself a literal.
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

/// The outcome of proving a string coercion (used by the checker to decide
/// between "no error / skip runtime check" and "hard error with witness").
pub enum Proof {
    /// Not applicable — this coercion is not a finite-string containment case;
    /// leave the ordinary runtime behaviour untouched.
    NotApplicable,
    /// Statically proven contained: the backends may skip the runtime check.
    Proven,
    /// An interpolation can produce `witness`, which is not in the target
    /// language — a compile error.
    Witness(String),
}

/// Prove (or refute) that `expr` flowing into the validated string type `to`
/// is contained. `resolve` types the interpolation holes (and, for the finite-
/// variable case, `expr` itself). This is the single decision procedure shared
/// by the checker's error path and the backends' skip path.
pub fn prove_string_flow(
    expr: &Expr,
    to: &Type,
    types: &HashMap<String, TypeDecl>,
    resolve: &dyn Fn(&Expr) -> Option<Type>,
) -> Proof {
    // The target must be a pure-regex validated string type; otherwise this is a
    // length/other predicate → runtime fallback.
    let Some(tdecl) = string_type_decl(to, types) else {
        return Proof::NotApplicable;
    };
    let Some(target) = regex_dfa_of_type(tdecl) else {
        return Proof::NotApplicable;
    };

    // Case 1: `expr` is an interpolation. Its language is exactly what it can
    // produce, so non-containment is a hard error (with a witness).
    if let Some(pieces) = flatten_template(expr) {
        if has_hole(&pieces) {
            match template_language(&pieces, types, resolve) {
                Some(l) => {
                    return match regex::contains(&target, &l) {
                        Ok(()) => Proof::Proven,
                        Err(witness) => Proof::Witness(witness),
                    }
                }
                // A hole is not finite ⇒ runtime validation, unchanged.
                None => return Proof::NotApplicable,
            }
        }
    }

    // Case 2: a plain expression of a finite string type S flowing into T. If
    // L(S) ⊆ L(T) we may skip the check; if not, the runtime check STAYS (a
    // variable may still hold a conforming value — never an error here).
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

/// The boolean form used by the backends: `true` iff the flow is statically
/// proven contained (so the runtime validation may be skipped). A `Witness`
/// outcome (impossible in a well-typed program the checker already accepted)
/// conservatively keeps the check.
pub fn string_flow_proven(
    expr: &Expr,
    to: &Type,
    types: &HashMap<String, TypeDecl>,
    resolve: &dyn Fn(&Expr) -> Option<Type>,
) -> bool {
    matches!(prove_string_flow(expr, to, types, resolve), Proof::Proven)
}

/// Enumerate the language of a finite string type up to `cap`, for LSP
/// completion. `None` if the type is not a finite string type or has more than
/// `cap` members.
pub fn enumerate_type(decl: &TypeDecl, cap: usize) -> Option<Vec<String>> {
    regex_dfa_of_type(decl)?.enumerate(cap)
}
