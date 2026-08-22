//! Structural type resolution shared by the checker and the codegen backends.
//!
//! `resolve` reduces a [`Type`] to its underlying representation: a validated
//! `Named` type decays to its scalar base, a named record to its `Record`, and
//! the compile-time transformers `Omit`/`Pick`/`Merge` (RFC-0002 §7) evaluate to
//! a concrete `Record`. Transformers are therefore fully erased before codegen.

use std::collections::HashMap;

use crate::ast::*;
use crate::codec::Wire;

/// The **structural identity** of a type — 64 bits of SHA-256 over its whole
/// shape, as hex. One definition, because it was three.
///
/// # Why a synthesized name needs one
///
/// A generated function or symbol is BOTH a name and the key its worklist dedups
/// on, so two distinct types that produce one string mean the second body is
/// skipped and both sites point at the first. `vyrn-codegen`'s `mangle_ty` is a
/// READABLE spelling and it is not injective — `Option<Int64>` and a user type
/// named `OptInt64` both spell `OptInt64`, every structural record spells `Rec`,
/// every `Omit`/`Pick`/`Merge`/`Partial` spells `Xf` — and RFC-0077 M2e is the
/// bug that found it: native read one instantiation's value through another's
/// body while `vyrn check` printed `ok`. The three synthesizers that name a
/// function after a type (`mangle_name`/`stream_close_sym`/`mangle_dispatch_sym`
/// in codegen, [`crate::jsonenc::enc_name`], [`crate::jsondec::top_name`]) each
/// append this, so injectivity is one claim in one place.
///
/// # Why the derived `Debug` form is the structural serialization
///
/// [`Type`] is a plain tree of `String`/`Vec`/`Box`/integers with no map, no
/// address and no interior mutability in it, so `{:?}` is a total, deterministic
/// rendering of the whole shape, and it is injective because `Debug` for `String`
/// quotes and escapes. Hand-writing an equivalent walk would be forty lines whose
/// own injectivity then needed the argument this one gets from the derive —
/// separators are exactly what `mangle_ty` got wrong.
///
/// What `Debug` is NOT is a stable format anyone promised: a field rename or a
/// changed derive moves every key at once. That costs nothing as long as no
/// artifact OUTSIDE the emitted module names one, which is the standing condition
/// on this function — and it holds today in both directions:
///
/// - codegen's symbols: an `export extern fn` is exported under its own source
///   name, and the exported symbols are never generic;
/// - the JSON codec's function names: the reserved `json$e…`/`json$t…` spellings
///   live entirely inside one linked program. The JSON **wire format** does not
///   contain them — RFC-0018's bytes are field names and values — so no
///   `.json` file, fixture, `vyrn.lock`, `docs/api` page or cached blob carries
///   one, and nothing on disk pins one.
///
/// `struct_key_is_pinned` in this module's tests holds that condition to a
/// COMMITTED table: the rendering cannot change silently, in this process or a
/// later build, because a changed key is a red test naming the type it moved.
/// If a future artifact ever does carry one of these names, that test is the
/// place the compatibility break becomes visible.
///
/// # 64 bits
///
/// A birthday bound, not a proof: *n* distinct types collide with probability
/// about *n*²/2⁶⁵, which is 2.7 × 10⁻⁸ at a million distinct types in one
/// program — several orders past any real one, and `Debug` strings are not
/// adversarial input. The remaining risk is not argued away, it is DETECTED:
/// [`crate::jsonenc`] and [`crate::jsondec`] memoize on the key and keep the
/// type beside it, so a collision is a build error naming both types instead of
/// an encoder that writes the wrong shape.
pub fn struct_key(x: &impl std::fmt::Debug) -> String {
    crate::hash::sha256_hex(format!("{x:?}").as_bytes())[..16].to_string()
}

/// Guards against cyclic type aliases (e.g. `type A = Omit<A, x>`), which would
/// otherwise recurse forever. A resolution deeper than this yields `Unit`, which
/// surfaces as a type error downstream rather than a stack overflow.
const MAX_DEPTH: usize = 64;

/// The target type of a numeric conversion `Name(x)` (e.g. `Int32(x)`,
/// `Float64(x)`), or `None` if `name` is not a numeric type name. Conversions
/// resize/round between `Int`, sized `IntN`, and `Float` (sext/trunc/sitofp/fptosi).
pub fn numeric_conv_target(name: &str) -> Option<Type> {
    match name {
        // Only the sized spellings exist — there is no `Int(x)`/`Float(x)`.
        "Int64" => Some(Type::Int),
        "Int32" => Some(Type::IntN {
            bits: 32,
            signed: true,
        }),
        "Int16" => Some(Type::IntN {
            bits: 16,
            signed: true,
        }),
        "Int8" => Some(Type::IntN {
            bits: 8,
            signed: true,
        }),
        "UInt8" => Some(Type::IntN {
            bits: 8,
            signed: false,
        }),
        "UInt16" => Some(Type::IntN {
            bits: 16,
            signed: false,
        }),
        "UInt32" => Some(Type::IntN {
            bits: 32,
            signed: false,
        }),
        "UInt64" => Some(Type::IntN {
            bits: 64,
            signed: false,
        }),
        "Float64" => Some(Type::Float),
        "Float32" => Some(Type::Float32),
        _ => None,
    }
}

/// `I32x4`'s lane type (RFC-0083 M3), spelled once for all three engines.
///
/// SIGNED, and that is the whole of the signedness decision: wasm offers
/// `i32x4.min_s`/`min_u` and a signed and an unsigned form of every comparison,
/// and the lane type picks one half of each pair rather than the operation
/// naming it. The other half belongs to a `U32x4` that is not proposed — the
/// choice is the operand's, so a second set of spellings on one type would be
/// two answers to a question the type already answered.
pub const INT32: Type = Type::IntN {
    bits: 32,
    signed: true,
};

/// The lane `v.lane(k)` reads, or `None` when `k` is not a compile-time constant
/// in `0..lanes` (RFC-0083).
///
/// ONE copy of the rule because it is the reason no bounds check is emitted
/// anywhere: the checker refuses what the three backends would otherwise have to
/// trap on, so the operation stays total, and all four ask this same question.
/// `consteval` already answers it for refinement predicates.
pub fn const_lane(idx: &Expr, lanes: i64) -> Option<u8> {
    match crate::consteval::eval(idx, &HashMap::new()) {
        Some(crate::consteval::ConstVal::Int(k)) if k >= 0 && k < lanes => Some(k as u8),
        _ => None,
    }
}

/// A canonical key for a type used as a protocol-impl target (RFC-0002 §5).
/// Only the types whose runtime value carries enough to dispatch on are
/// supported in v1: the scalars and named types (validated scalars, enums).
/// Records and other structural types return `None` (no runtime identity).
///
/// A generic type keys on its **constructor alone** (RFC-0080 M1):
/// `Option<Int64>` and `Option<String>` both key `Option`, so one
/// `impl<T> Show for Option<T>` serves every instantiation and the receiver's
/// type arguments are recovered by unification at the call site instead of by
/// the key. That is also what makes overlap a declaration-time question — two
/// impls for the same constructor collide on the key whether or not their
/// arguments differ, which is exactly the "one impl per (protocol, type
/// constructor)" rule.
pub fn type_key(ty: &Type) -> Option<String> {
    match ty {
        Type::Int => Some("Int64".to_string()),
        Type::Bool => Some("Bool".to_string()),
        Type::Str => Some("String".to_string()),
        Type::Named(n) => Some(n.clone()),
        Type::Option(_) => Some("Option".to_string()),
        Type::Result(..) => Some("Result".to_string()),
        Type::App(n, _) => Some(n.clone()),
        _ => None,
    }
}

/// The internal (mangled) function name for an `impl` method, e.g.
/// `Show__Int__show`. Unique per (protocol, target type, method).
pub fn impl_method_name(protocol: &str, type_key: &str, method: &str) -> String {
    format!("{protocol}__{type_key}__{method}")
}

/// The protocol `?` resolves through for an operand that is neither `Option`
/// nor `Result` (RFC-0080 M3). Declared in `std/fallible.vyrn`; the compiler
/// knows only the name and the two method names below, so a program that
/// declares the protocol itself works identically to one that imports it.
///
/// **`Option` and `Result` deliberately do NOT route through it**, and that is
/// the part of M3 that was refused rather than the part that was forgotten.
/// Four reasons, in the order they bite:
///
/// 1. `vyrn run` on a bare file has no resolver and therefore no `std/`
///    (`Interp`'s tests are exactly this). Routing `x?` on an `Option` through
///    a std protocol would make the most common operator in the language
///    depend on a module lookup that is allowed to fail.
/// 2. `?` on a `Result` checks `assignable(e, re)` — the error types must
///    match. `Fallible` has one associated type and it is the *success*
///    payload; the check has nowhere to live.
/// 3. The two nominal arms produce specific diagnostics ("`?` propagates error
///    {e}, but the function returns Result<_, {re}>"). A protocol path can only
///    say the operand does not implement `Fallible`.
/// 4. `Option`/`Result` lower `?` to a tag test and an `extractvalue`, inline.
///    Going through the protocol makes every `?` in the corpus two calls whose
///    bodies re-`match` the value the branch already tested — a real cost on an
///    operator `std/json`, `std/scan` and `std/num` use in loops.
///
/// So the operator is nominal for the two shapes the language builds in, and
/// open for everything else. What M3 actually delivers is the second half.
pub const FALLIBLE: &str = "Fallible";

/// The protocol a type implements to say **it owns heap and here is how to
/// release it** (RFC-0086 M1). The compiler knows the name and the one method
/// name; everything else about it is an ordinary declaration, so a program that
/// declares `protocol Owned { fn release(self) }` itself works identically to one
/// that imports it — the same bootstrap answer [`FALLIBLE`] gives above, for the
/// same reason (`vyrn run` on a bare file has no resolver).
pub const OWNED: &str = "Owned";

/// The one method [`OWNED`] declares.
pub const OWNED_RELEASE: &str = "release";

/// The protocol a type implements to say **a value of it must be disposed of
/// by name** (RFC-0086 M3) — the linear obligation RFC-0075 built for `Stream`
/// and never exposed.
///
/// It declares **no methods**, and that is the design rather than an omission.
/// The obligation is a fact about the type, not a behaviour: `movecheck` proves
/// that every path either hands the value on or releases it, and the release
/// itself is already declared — by [`OWNED`], or by nothing where the type owns
/// no heap. A method here would be a second `release`.
///
/// **It is not `consume`, and RFC-0087 U7 asked whether it should be.** They
/// answer different questions. `consume` is a *calling convention*: who owns
/// this argument after the call. `MustUse` is an *obligation on a type*: this
/// value must be disposed of by name, wherever it goes. A `String` is
/// consumable and carries no obligation; a `Stream` carries one however any
/// particular function takes it. Merging them would make every `consume`
/// parameter a linear value and every linear value a calling convention, and
/// neither implication is true.
///
/// Known by name for the reason [`OWNED`] is: `vyrn run` on a bare file has no
/// resolver, so a decision the compiler refuses programs over may not depend on
/// a module lookup.
pub const MUST_USE: &str = "MustUse";

/// The protocol a type implements to say **how it is duplicated** (RFC-0091
/// M1). `x.copy()` is structural for every type whose parts copy; a type with
/// an invariant a structural copy would break declares this instead.
///
/// Known by name for the reason [`OWNED`] is: `vyrn run` on a bare file has no
/// resolver, so the decision that allocates may not depend on a module lookup.
pub const COPY: &str = "Copy";

/// The one method [`COPY`] declares.
pub const COPY_COPY: &str = "copy";

/// The protocol a type implements to say **how it renders as text** (RFC-0094
/// M3). RFC-0007 §v2 wrote this deferral by name: "letting user types be
/// interpolable via a `Display` protocol".
///
/// `print`, `x.toString()` (`@str`) and every interpolation hole (`value`) took
/// a union of the scalars the compiler renders, and refused everything else. A
/// type outside that union now asks its declaration.
///
/// **The seed is the existing lowering, not a row, and that is the one place
/// this differs from [`COPY`] and [`OWNED`].** A scalar never reaches the
/// dispatch, so no `impl Show for Int64` can change what `7` prints as. Two
/// measured reasons, not one preference:
///
/// 1. `examples/protocol.vyrn` declares `impl Show for Int64` whose body is
///    `self.toString()`. Under "the declaration always wins" that is infinite
///    recursion, in a file that compiles today.
/// 2. Parity compares bytes, and float rendering is where it breaks. One
///    lowering renders a `Float64` in three engines (`std/num`'s `f64Str`); a
///    seeded row a program could replace would be a second answer to keep in
///    step with it.
///
/// So the rule is additive: **a scalar renders by the language's lowering, and
/// a type the language cannot render asks its declaration.** Nothing that
/// compiles today changes what it prints.
///
/// Known by name for the reason [`OWNED`] is: `vyrn run` on a bare file has no
/// resolver, so `print` may not depend on a module lookup.
pub const SHOW: &str = "Show";

/// The one method [`SHOW`] declares.
pub const SHOW_SHOW: &str = "show";

/// Whether the language renders `t` itself.
///
/// These are exactly the scalars `print`, `@str` and the three backends already
/// lower, and they are the union RFC-0094 calls "the union parameter". A type
/// here never reaches [`show_impl`]; everything else may.
pub fn renders(t: &Type) -> bool {
    matches!(
        t,
        Type::Int | Type::IntN { .. } | Type::Float | Type::Float32 | Type::Bool | Type::Str
    )
}

/// The `impl Show for T` method a value of type `ty` renders through, or `None`
/// where nothing declared one. The caller has already established that
/// [`renders`] is false for the resolved type.
pub fn show_impl(impls: &[ImplBlock], ty: &Type) -> Option<String> {
    show_impl_by_key(impls, &type_key(ty)?)
}

/// [`show_impl`], by type key. The interpreter reaches this one, for the reason
/// [`copy_impl_by_key`] exists: it dispatches on a runtime value, whose key is
/// the name stamped on it.
pub fn show_impl_by_key(impls: &[ImplBlock], key: &str) -> Option<String> {
    impls
        .iter()
        .any(|i| {
            i.protocol == SHOW
                && type_key(&i.ty).as_deref() == Some(key)
                && i.methods.iter().any(|m| m.name == SHOW_SHOW)
        })
        .then(|| impl_method_name(SHOW, key, SHOW_SHOW))
}

/// The protocol a container implements to say **how it is iterated** (RFC-0091
/// M3): `type Item`, `fn size(read self) -> Int64`, and
/// `place nth(read self, i: Int64) -> Item`.
///
/// `for x in xs` reaches a builtin container through the compiler's own element
/// walk and a user container through this row. There is no second list: the
/// lookup is `place nth` under the receiver's type key, which is the same table
/// `a[i]` reads (see [`crate::project`]).
pub const ITERATE: &str = "Iterate";

/// The counting method [`ITERATE`] declares.
pub const ITERATE_SIZE: &str = "size";

/// The projection [`ITERATE`] declares — a `place`, not a method.
pub const ITERATE_NTH: &str = "nth";

/// The `impl Copy for T` method a receiver of type `ty` dispatches to
/// (RFC-0091 M1), or `None` where the copy stays structural.
///
/// The declaration wins over the derivation, exactly as an `impl Owned` row
/// wins over the seeded release: a type that states what duplicating it means
/// is what `copy` means for it.
pub fn copy_impl(impls: &[ImplBlock], ty: &Type) -> Option<String> {
    copy_impl_by_key(impls, &type_key(ty)?)
}

/// [`copy_impl`], by type key. The interpreter reaches this one: it dispatches
/// on a runtime value, whose key is the name stamped on it.
pub fn copy_impl_by_key(impls: &[ImplBlock], key: &str) -> Option<String> {
    impls
        .iter()
        .any(|i| {
            i.protocol == COPY
                && type_key(&i.ty).as_deref() == Some(key)
                && i.methods.iter().any(|m| m.name == COPY_COPY)
        })
        .then(|| impl_method_name(COPY, key, COPY_COPY))
}

/// The `impl Owned for T` method a value with this type key releases through
/// (RFC-0086 M1), or `None` where nothing declared one.
///
/// [`crate::own::Owned`] keys the same rows by the same key and is where the
/// automatic path reads them. This one exists for the interpreter, which
/// dispatches an explicit `drop x` on a runtime value whose key is the name
/// stamped on it — the same route [`copy_impl_by_key`] takes, for the same
/// reason.
pub fn owned_impl_by_key(impls: &[ImplBlock], key: &str) -> Option<String> {
    impls
        .iter()
        .any(|i| {
            i.protocol == OWNED
                && type_key(&i.ty).as_deref() == Some(key)
                && i.methods.iter().any(|m| m.name == OWNED_RELEASE)
        })
        .then(|| impl_method_name(OWNED, key, OWNED_RELEASE))
}

/// The `impl Iterate for T` rows a receiver of type `ty` iterates through
/// (RFC-0091 M3): the flattened `size` name, and the `place nth` to inline.
///
/// Both halves are required. An impl carrying only one of them is not an
/// iterable, and the checker says so where it is written.
pub fn iterate_impl<'a>(impls: &'a [ImplBlock], ty: &Type) -> Option<(String, &'a Function)> {
    iterate_impl_by_key(impls, &type_key(ty)?)
}

/// [`iterate_impl`], by type key. The interpreter reaches this one: it names a
/// receiver by the stamp on its runtime value where no static type says.
pub fn iterate_impl_by_key<'a>(
    impls: &'a [ImplBlock],
    key: &str,
) -> Option<(String, &'a Function)> {
    let imp = impls
        .iter()
        .find(|i| i.protocol == ITERATE && type_key(&i.ty).as_deref() == Some(key))?;
    let nth = imp.places.iter().find(|f| f.name == ITERATE_NTH)?;
    imp.methods.iter().find(|m| m.name == ITERATE_SIZE)?;
    Some((impl_method_name(ITERATE, key, ITERATE_SIZE), nth))
}

/// Extract the `(min, max)` inclusive numeric bounds a validated type's `where`
/// predicate implies (RFC-0003 reflection). Recognizes `value >=/> N`,
/// `value <=/< N` in either operand order, and `&&` conjunctions. `N` may be a
/// negated literal (`value >= -5` parses as `Unary::Neg` over the literal) or a
/// byte literal (RFC-0057). Anything else (e.g. `value % 2 == 0`) contributes
/// no bound.
pub fn predicate_bounds(pred: &Expr) -> (Option<i64>, Option<i64>) {
    if let Expr::Binary { op, lhs, rhs, .. } = pred {
        if *op == BinOp::And {
            let (l0, l1) = predicate_bounds(lhs);
            let (r0, r1) = predicate_bounds(rhs);
            return (l0.or(r0), l1.or(r1));
        }
        // `value OP n` or `n OP value` → normalize to `value OP n`, reading `n`
        // through [`int_lit`] so a negated literal counts like the parser
        // spelled it.
        let (normalized, n) = match (&**lhs, &**rhs) {
            (l, r) if is_value(l) => (*op, int_lit(r)),
            (l, r) if is_value(r) => (flip(*op), int_lit(l)),
            _ => return (None, None),
        };
        let Some(n) = n else { return (None, None) };
        return match normalized {
            BinOp::GtEq => (Some(n), None),
            // An exclusive bound steps to its inclusive neighbor; the step
            // saturates at the `i64` edges, which is correct for an inclusive
            // bound (nothing beyond the edge was representable anyway).
            BinOp::Gt => (Some(n.saturating_add(1)), None),
            BinOp::LtEq => (None, Some(n)),
            BinOp::Lt => (None, Some(n.saturating_sub(1))),
            _ => (None, None),
        };
    }
    (None, None)
}

/// `n OP value` is equivalent to `value FLIP(OP) n`.
fn flip(op: BinOp) -> BinOp {
    match op {
        BinOp::Lt => BinOp::Gt,
        BinOp::Gt => BinOp::Lt,
        BinOp::LtEq => BinOp::GtEq,
        BinOp::GtEq => BinOp::LtEq,
        other => other,
    }
}

/// The `multipleOf` a predicate implies: `value % K == 0` (in a conjunction).
pub fn predicate_multiple_of(pred: &Expr) -> Option<i64> {
    if let Expr::Binary { op, lhs, rhs, .. } = pred {
        match op {
            BinOp::And => return predicate_multiple_of(lhs).or_else(|| predicate_multiple_of(rhs)),
            BinOp::Eq => {
                if let Expr::Binary {
                    op: BinOp::Rem,
                    lhs: base,
                    rhs: k,
                    ..
                } = &**lhs
                {
                    if matches!(&**base, Expr::Var { name, .. } if name == "value")
                        && matches!(&**rhs, Expr::Int(0))
                    {
                        if let Expr::Int(kv) = &**k {
                            return Some(*kv);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// The inclusive `(minLength, maxLength)` a predicate implies via
/// `value.byteLength OP N` comparisons (exclusive bounds are floored/ceiled to
/// inclusive, exactly like the JSON Schema emitter).
pub fn predicate_length_bounds(pred: &Expr) -> (Option<i64>, Option<i64>) {
    if let Expr::Binary { op, lhs, rhs, .. } = pred {
        if *op == BinOp::And {
            let (l0, l1) = predicate_length_bounds(lhs);
            let (r0, r1) = predicate_length_bounds(rhs);
            return (l0.or(r0), l1.or(r1));
        }
        let (norm, n) = match (&**lhs, &**rhs) {
            (l, r) if is_length_of_value(l) => (*op, int_lit(r)),
            (l, r) if is_length_of_value(r) => (flip(*op), int_lit(l)),
            _ => return (None, None),
        };
        if let Some(n) = n {
            return match norm {
                BinOp::GtEq => (Some(n), None),
                // The same saturating step `predicate_bounds` takes: a literal
                // at the `i64` edge must not overflow the reflection.
                BinOp::Gt => (Some(n.saturating_add(1)), None),
                BinOp::LtEq => (None, Some(n)),
                BinOp::Lt => (None, Some(n.saturating_sub(1))),
                _ => (None, None),
            };
        }
    }
    (None, None)
}

/// The first `value =~ "…"` pattern in a predicate conjunction, as written
/// (unanchored — the anchoring is the JSON Schema emitter's concern).
pub fn predicate_pattern(pred: &Expr) -> Option<String> {
    if let Expr::Binary { op, lhs, rhs, .. } = pred {
        match op {
            BinOp::And => return predicate_pattern(lhs).or_else(|| predicate_pattern(rhs)),
            BinOp::Match => {
                if matches!(&**lhs, Expr::Var { name, .. } if name == "value") {
                    if let Expr::Str(pat) = &**rhs {
                        return Some(pat.clone());
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// The surface spelling of a schema base type (what `Schema.base` reports).
fn base_spelling(ty: &Type) -> &'static str {
    match ty {
        Type::Int => "Int64",
        Type::IntN {
            bits: 8,
            signed: true,
        } => "Int8",
        Type::IntN {
            bits: 16,
            signed: true,
        } => "Int16",
        Type::IntN {
            bits: 32,
            signed: true,
        } => "Int32",
        Type::IntN {
            bits: 8,
            signed: false,
        } => "UInt8",
        Type::IntN {
            bits: 16,
            signed: false,
        } => "UInt16",
        Type::IntN {
            bits: 32,
            signed: false,
        } => "UInt32",
        Type::IntN {
            bits: 64,
            signed: false,
        } => "UInt64",
        Type::IntN { .. } => "?",
        Type::Float => "Float64",
        Type::Float32 => "Float32",
        Type::Bool => "Bool",
        Type::Str => "String",
        Type::Record(_) => "record",
        Type::Enum(_) => "enum",
        _ => "?",
    }
}

/// Build the `Schema { .. }` struct-literal expression for a declared type —
/// the compile-time reflection of `schemaOf(TypeName)`: the type's name, base
/// spelling, `///` doc, and everything its `where` predicate implies (numeric
/// bounds, `multipleOf`, string length bounds, regex pattern). Both backends
/// evaluate the *same* expression, so the invariant holds by construction.
pub fn schema_struct_lit(decl: &TypeDecl) -> Expr {
    let pred = decl.predicate.as_ref();
    let (min, max) = pred.map_or((None, None), |p| predicate_bounds(p));
    let (min_len, max_len) = pred.map_or((None, None), |p| predicate_length_bounds(p));
    let multiple_of = pred.and_then(predicate_multiple_of);
    let pattern = pred.and_then(predicate_pattern);
    let opt = |n: Option<i64>| match n {
        Some(v) => Expr::Call {
            name: "Some".to_string(),
            args: vec![Expr::Int(v)],
            line: 0,
        },
        None => Expr::Var {
            name: "None".to_string(),
            line: 0,
        },
    };
    let opt_str = |s: Option<String>| match s {
        Some(v) => Expr::Call {
            name: "Some".to_string(),
            args: vec![Expr::Str(v)],
            line: 0,
        },
        None => Expr::Var {
            name: "None".to_string(),
            line: 0,
        },
    };
    Expr::StructLit {
        name: "Schema".to_string(),
        fields: vec![
            ("name".to_string(), Expr::Str(decl.name.clone())),
            (
                "base".to_string(),
                Expr::Str(base_spelling(&decl.base).to_string()),
            ),
            ("doc".to_string(), opt_str(decl.doc.clone())),
            ("min".to_string(), opt(min)),
            ("max".to_string(), opt(max)),
            ("multipleOf".to_string(), opt(multiple_of)),
            ("minLength".to_string(), opt(min_len)),
            ("maxLength".to_string(), opt(max_len)),
            ("pattern".to_string(), opt_str(pattern)),
        ],
        line: 0,
    }
}

/// Render a complete JSON Schema (draft 2020-12) document for a declared type as a
/// compile-time-constant string — the reflection behind `jsonSchema(TypeName)`.
/// Both backends emit this *identical* string (see `schema_struct_lit` for the same
/// technique), so interpreter/native parity holds by construction.
///
/// Scalars map to the standard type names (`integer`/`number`/`string`/`boolean`);
/// a validated type's `where` predicate contributes `minimum`/`maximum`/
/// `exclusiveMinimum`/`exclusiveMaximum`/`multipleOf`; a record maps to an
/// `object` with `properties` and a `required` list (non-`Option` fields).
pub fn json_schema_string(decl: &TypeDecl, types: &HashMap<String, TypeDecl>) -> String {
    let dialect = "\"$schema\":\"https://json-schema.org/draft/2020-12/schema\"";
    let mut cx = SchemaCx {
        types,
        root: &decl.name,
        defs: Vec::new(),
    };
    // Render the root's body directly — inside the expansion, `Named(root)`
    // is a back-edge and renders as `{"$ref":"#"}`.
    let inner = named_schema(decl, &mut cx);
    // Nested named types were rendered into `$defs`; splice them in as the
    // root object's last member (the schemastore convention, and what the
    // RFC-0010 importer synthesizes back byte-identically).
    let body = if cx.defs.is_empty() {
        inner
    } else {
        let defs: Vec<String> = cx
            .defs
            .iter()
            .map(|(n, s)| format!("\"{}\":{}", json_escape(n), s.as_deref().unwrap_or("{}")))
            .collect();
        format!(
            "{},\"$defs\":{{{}}}}}",
            &inner[..inner.len() - 1],
            defs.join(",")
        )
    };
    if body == "{}" {
        format!("{{{dialect}}}")
    } else {
        // Splice the dialect in as the first member (drop `body`'s leading `{`).
        format!("{{{dialect},{}", &body[1..])
    }
}

/// Shared state of one `json_schema_string` rendering.
struct SchemaCx<'a> {
    types: &'a HashMap<String, TypeDecl>,
    /// The root declaration's name — a back-edge to it renders `{"$ref":"#"}`.
    root: &'a str,
    /// Every non-root named type encountered, in first-encounter order:
    /// `(name, rendered schema)`. A `None` body means "currently rendering" —
    /// a cycle just takes the `$ref`, which is what makes recursive types
    /// (mutually or through the root) terminate naturally.
    defs: Vec<(String, Option<String>)>,
}

/// The JSON Schema object (`{ .. }`) for a structural type, without the top-level
/// `$schema` dialect. A named type renders as `{"$ref":"#/$defs/N"}` (the root
/// itself as `{"$ref":"#"}`) with its schema collected into [`SchemaCx::defs`] —
/// except synthetic inline refinement helpers (`User.age`), which stay inlined so
/// a field-level `where` round-trips as the inline constraints the user wrote.
///
/// Past the name, the SHAPE is not decided here: [`crate::codec::wire`] answers
/// what a type is on the wire, and this function only spells that answer as JSON
/// Schema. It used to answer for itself, over the raw type, with an open `_` arm
/// — so `Omit<User, password>`, an applied generic, and every other type whose
/// form comes from resolving one were described as `{}` ("anything") while the
/// encoder wrote them out in full.
fn type_schema(ty: &Type, cx: &mut SchemaCx) -> String {
    match ty {
        Type::Named(n) => {
            if n == cx.root {
                return "{\"$ref\":\"#\"}".to_string();
            }
            let types = cx.types;
            // Synthetic inline-refinement helpers are desugaring artifacts,
            // not schema-worthy names — render their body inline.
            if n.contains('.') {
                return match types.get(n) {
                    Some(d) => named_schema(d, cx),
                    None => "{}".to_string(),
                };
            }
            match types.get(n) {
                Some(d) => {
                    if !cx.defs.iter().any(|(dn, _)| dn == n) {
                        let i = cx.defs.len();
                        cx.defs.push((n.clone(), None)); // in progress
                        let body = named_schema(d, cx);
                        cx.defs[i].1 = Some(body);
                    }
                    format!("{{\"$ref\":\"#/$defs/{}\"}}", json_escape(n))
                }
                None => "{}".to_string(),
            }
        }
        // A schema DESCRIBES what is written, so it asks the encode direction:
        // a `lazy T` field is a `T` in the JSON and nothing about the deferral is
        // visible to a client.
        _ => match crate::codec::wire(ty, cx.types, false) {
            // The default Int64 is "just an integer"; a *sized* int is a
            // deliberate wire-width choice, so its bounds are part of the
            // contract.
            Ok(Wire::Int) => "{\"type\":\"integer\"}".to_string(),
            Ok(Wire::IntN { bits, signed }) => intn_schema(bits, signed, &[]),
            Ok(Wire::Float | Wire::Float32) => "{\"type\":\"number\"}".to_string(),
            Ok(Wire::Bool) => "{\"type\":\"boolean\"}".to_string(),
            Ok(Wire::Str) => "{\"type\":\"string\"}".to_string(),
            // An `Option<T>` field carries `T`'s schema; its optionality is
            // expressed by omission from the enclosing object's `required` list.
            Ok(Wire::Option(inner)) => type_schema(&inner, cx),
            Ok(Wire::Array(inner) | Wire::FixedArray(inner, _)) => {
                format!(
                    "{{\"type\":\"array\",\"items\":{}}}",
                    type_schema(&inner, cx)
                )
            }
            // A `Map<String, V>` (RFC-0028) is a free-form JSON object whose
            // values all share `V`'s schema: `additionalProperties` carries it.
            Ok(Wire::Map(val)) => {
                format!(
                    "{{\"type\":\"object\",\"additionalProperties\":{}}}",
                    type_schema(&val, cx)
                )
            }
            Ok(Wire::Record(fields)) => record_schema(&fields, cx),
            // A payload-less sum type is exactly a JSON Schema `enum` of its
            // variant names. A payload sum type — and `Result<T, E>`, which IS
            // one on the wire — emits the RFC-0024 externally-tagged `oneOf`,
            // which the importer recognizes back into an enum decl.
            Ok(Wire::Enum(variants)) => {
                if variants.iter().all(|v| v.payload.is_empty()) {
                    let names: Vec<String> = variants
                        .iter()
                        .map(|v| format!("\"{}\"", json_escape(&v.name)))
                        .collect();
                    format!("{{\"enum\":[{}]}}", names.join(","))
                } else {
                    enum_oneof_schema(&variants, cx)
                }
            }
            // A type with no wire form has no schema to describe it either. `{}`
            // is JSON Schema's "anything", which is the honest answer for a value
            // that never becomes JSON.
            Err(_) => "{}".to_string(),
        },
    }
}

/// The RFC-0024 `oneOf` schema for a payload enum: nullary variants as
/// `{"const":"Name"}`, single-payload variants as a one-property object, tuple
/// payloads via `prefixItems` + `"items":false` (the honest draft-2020-12 tuple
/// form the importer round-trips). Variant order is declaration order.
fn enum_oneof_schema(variants: &[EnumVariant], cx: &mut SchemaCx) -> String {
    let branches: Vec<String> = variants
        .iter()
        .map(|v| {
            let name = json_escape(&v.name);
            match v.payload.len() {
                0 => format!("{{\"const\":\"{name}\"}}"),
                1 => {
                    let p = type_schema(&v.payload[0], cx);
                    format!(
                        "{{\"type\":\"object\",\"properties\":{{\"{name}\":{p}}},\"required\":[\"{name}\"]}}"
                    )
                }
                _ => {
                    let items: Vec<String> =
                        v.payload.iter().map(|p| type_schema(p, cx)).collect();
                    let tuple = format!(
                        "{{\"type\":\"array\",\"prefixItems\":[{}],\"items\":false}}",
                        items.join(",")
                    );
                    format!(
                        "{{\"type\":\"object\",\"properties\":{{\"{name}\":{tuple}}},\"required\":[\"{name}\"]}}"
                    )
                }
            }
        })
        .collect();
    format!("{{\"oneOf\":[{}]}}", branches.join(","))
}

/// The schema for a sized integer: `"integer"` plus its width bounds, merged
/// with any predicate-derived constraints in `extra` (a predicate bound on the
/// same key wins — it is checked against the width at runtime anyway, and a
/// JSON object cannot repeat a key). `UInt64`'s maximum (2^64 − 1) exceeds
/// what a Vyrn `where` clause (an `Int64` literal) can express on re-import,
/// so only its `minimum` is emitted.
fn intn_schema(bits: u8, signed: bool, extra: &[(String, String)]) -> String {
    const BOUND_KEYS: [&str; 4] = ["minimum", "exclusiveMinimum", "maximum", "exclusiveMaximum"];
    let mut parts = vec!["\"type\":\"integer\"".to_string()];
    let has = |k: &str| extra.iter().any(|(ek, _)| ek == k);
    // Bound keywords in the importer's canonical clause order, so
    // emit → import → re-emit is byte-stable regardless of how the source
    // predicate ordered its comparisons. Width defaults fill a bound family
    // (min/max) only when the predicate left it open. Arithmetic shifts of
    // the i64 extremes are well-defined for every width (a plain `1 << bits`
    // would overflow at 64).
    for key in BOUND_KEYS {
        if let Some((_, v)) = extra.iter().find(|(ek, _)| ek == key) {
            parts.push(format!("\"{key}\":{v}"));
        } else if key == "minimum" && !has("exclusiveMinimum") {
            let lo: i64 = if signed { i64::MIN >> (64 - bits) } else { 0 };
            parts.push(format!("\"minimum\":{lo}"));
        } else if key == "maximum" && !has("exclusiveMaximum") && !(bits == 64 && !signed) {
            let hi: i64 = if signed {
                i64::MAX >> (64 - bits)
            } else {
                (1i64 << bits) - 1
            };
            parts.push(format!("\"maximum\":{hi}"));
        }
    }
    for (k, v) in extra {
        if !BOUND_KEYS.contains(&k.as_str()) {
            parts.push(format!("\"{k}\":{v}"));
        }
    }
    format!("{{{}}}", parts.join(","))
}

/// The schema for a named declaration: a validated scalar carries its `where`
/// constraints; anything else defers to its structural base (record, alias, …).
fn named_schema(decl: &TypeDecl, cx: &mut SchemaCx) -> String {
    let pred = decl.predicate.as_ref();
    match &decl.base {
        Type::Int => scalar_with_constraints("integer", pred),
        // A refined sized int merges its width bounds with the predicate's
        // constraints (predicate wins on a shared key); a predicate the
        // keyword model can't fully capture is documented, as for scalars.
        Type::IntN { bits, signed } => {
            let mut cs = Vec::new();
            let complete = pred
                .map(|p| collect_constraints(p, &mut cs))
                .unwrap_or(true);
            let s = intn_schema(*bits, *signed, &cs);
            if complete {
                s
            } else {
                format!(
                    "{},{}}}",
                    &s[..s.len() - 1],
                    unmapped_comment(pred.unwrap())
                )
            }
        }
        Type::Float | Type::Float32 => scalar_with_constraints("number", pred),
        Type::Bool => "{\"type\":\"boolean\"}".to_string(),
        Type::Str => string_with_constraints(pred),
        // A record with a cross-field `where` reflects the object schema plus a
        // `$comment` naming the invariant (JSON Schema can't express arithmetic
        // between properties; the runtime check remains the source of truth).
        Type::Record(fields) if pred.is_some() => {
            let obj = record_schema(fields, cx);
            let comment = unmapped_comment(pred.unwrap());
            format!("{}{}}}", &obj[..obj.len() - 1], format!(",{comment}"))
        }
        other => type_schema(other, cx),
    }
}

/// `{"type":"string", <length constraints>}` — a `String` refinement expresses
/// bounds via `value.byteLength OP N` (→ `minLength`/`maxLength`) and `value =~ "…"`
/// (→ `pattern`). Two or more patterns are combined with `allOf` (a JSON object
/// has at most one `pattern`). A form the model can't capture is documented in a
/// `$comment` (as for scalars).
///
/// Length semantics (decided): Vyrn's `value.byteLength` counts **bytes** (native
/// `strlen`, interp `str::len`), while JSON Schema's `minLength`/`maxLength`
/// count Unicode code points. The two agree exactly on ASCII — which is what
/// length refinements are used for in practice (usernames, codes, keys). For
/// multi-byte text they diverge per bound: a code-point `maxLength` is looser
/// than Vyrn's byte check (every code point is ≥ 1 byte), a code-point
/// `minLength` can be stricter. Emitting the numbers unchanged is the honest
/// mapping; the runtime refinement remains the source of truth (the same
/// stance every `$comment` fallback takes).
fn string_with_constraints(pred: Option<&Expr>) -> String {
    let mut parts = vec!["\"type\":\"string\"".to_string()];
    if let Some(p) = pred {
        let mut cs = Vec::new();
        let complete = collect_string_constraints(p, &mut cs);
        // A JSON Schema object allows only one `pattern`; collect them apart so
        // several regex clauses can be `allOf`-combined instead of clashing.
        let patterns: Vec<String> = cs
            .iter()
            .filter(|(k, _)| k == "pattern")
            .map(|(_, v)| v.clone())
            .collect();
        for (k, v) in &cs {
            if k != "pattern" {
                parts.push(format!("\"{k}\":{v}"));
            }
        }
        match patterns.len() {
            0 => {}
            1 => parts.push(format!("\"pattern\":{}", patterns[0])),
            _ => {
                let branches: Vec<String> = patterns
                    .iter()
                    .map(|p| format!("{{\"pattern\":{p}}}"))
                    .collect();
                parts.push(format!("\"allOf\":[{}]", branches.join(",")));
            }
        }
        if !complete {
            parts.push(unmapped_comment(p));
        }
    }
    format!("{{{}}}", parts.join(","))
}

/// Collect `minLength`/`maxLength` from a `String` predicate over `value.byteLength`,
/// returning whether it was captured in full.
fn collect_string_constraints(pred: &Expr, out: &mut Vec<(String, String)>) -> bool {
    let Expr::Binary { op, lhs, rhs, .. } = pred else {
        return false;
    };
    if *op == BinOp::And {
        let a = collect_string_constraints(lhs, out);
        let b = collect_string_constraints(rhs, out);
        return a && b;
    }
    // `value =~ "pat"` → a JSON Schema `pattern`. Vyrn's `=~` is a full match, so
    // anchor it (`^…$`); the subset is a subset of ECMA-262 with identical meaning.
    if *op == BinOp::Match {
        if is_value(lhs) {
            if let Expr::Str(pat) = &**rhs {
                out.push((
                    "pattern".to_string(),
                    format!("\"{}\"", json_escape(&format!("^{pat}$"))),
                ));
                return true;
            }
        }
        return false;
    }
    // `value.byteLength OP N` or `N OP value.byteLength`. `>` and `>=` both floor the
    // length (JSON Schema minLength is inclusive), so `> N` becomes `N + 1`.
    let (norm, lit) = match (&**lhs, &**rhs) {
        (l, r) if is_length_of_value(l) => (*op, int_lit(r)),
        (l, r) if is_length_of_value(r) => (flip(*op), int_lit(l)),
        _ => return false,
    };
    match (norm, lit) {
        (BinOp::GtEq, Some(n)) => push_true(out, "minLength", n.to_string()),
        (BinOp::Gt, Some(n)) => push_true(out, "minLength", n.saturating_add(1).to_string()),
        (BinOp::LtEq, Some(n)) => push_true(out, "maxLength", n.to_string()),
        (BinOp::Lt, Some(n)) => push_true(out, "maxLength", n.saturating_sub(1).to_string()),
        _ => false,
    }
}

/// True if `e` is `value.byteLength` (RFC-0058 renamed it from `value.byteLength`).
fn is_length_of_value(e: &Expr) -> bool {
    matches!(e, Expr::Field { expr, field, .. } if field == "byteLength" && is_value(expr))
}

/// An integer literal (possibly negated) as an `i64`, or `None`. A byte literal
/// (RFC-0057) counts too — it is an integer literal.
fn int_lit(e: &Expr) -> Option<i64> {
    match e {
        Expr::Int(n) => Some(*n),
        Expr::Byte(b) => Some(*b as i64),
        Expr::Unary {
            op: UnOp::Neg,
            expr,
            ..
        } => match &**expr {
            Expr::Int(n) => Some(-n),
            Expr::Byte(b) => Some(-(*b as i64)),
            _ => None,
        },
        _ => None,
    }
}

/// `{"type": tyname, <constraints from the where predicate>}`. A predicate the
/// keyword model can't fully encode (e.g. a disjunction) still emits the parts it
/// can, plus a `$comment` with the *exact* source predicate so the schema never
/// silently under-specifies — the runtime refinement remains the source of truth.
fn scalar_with_constraints(tyname: &str, pred: Option<&Expr>) -> String {
    let mut parts = vec![format!("\"type\":\"{tyname}\"")];
    if let Some(p) = pred {
        let mut cs = Vec::new();
        let complete = collect_constraints(p, &mut cs);
        for (k, v) in cs {
            parts.push(format!("\"{k}\":{v}"));
        }
        if !complete {
            parts.push(unmapped_comment(p));
        }
    }
    format!("{{{}}}", parts.join(","))
}

/// A `"$comment"` member naming the full source predicate — appended when the
/// schema keywords don't capture it exactly.
fn unmapped_comment(pred: &Expr) -> String {
    let text = format!("constrained by: {}", crate::checker::pred_summary(pred));
    format!("\"$comment\":\"{}\"", json_escape(&text))
}

/// Escape a string for embedding as a JSON string value.
///
/// One table, [`crate::codec::escape_into`] — the one both backends encode
/// with, and the one this crate's strict reader decodes. A second table here
/// escaped four characters of six and emitted a raw `\r` (and every other
/// control byte) into a schema, which RFC 8259 forbids and [`crate::codec`]'s
/// own reader rejects: `jsonSchema` wrote a document this repo cannot read.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    crate::codec::escape_into(s, &mut out);
    out
}

/// A record maps to a JSON Schema `object`; non-`Option` fields are `required`.
fn record_schema(fields: &[Field], cx: &mut SchemaCx) -> String {
    // A `lazy T` field is a `T` on the wire (RFC-0085 M4a): the schema describes
    // what `toJson` writes, and `toJson` forces. Nothing about the deferral is
    // visible to a client, which is the point of putting it on the field.
    let props: Vec<String> = fields
        .iter()
        .map(|f| format!("\"{}\":{}", f.name, type_schema(&forced(&f.ty), cx)))
        .collect();
    let required: Vec<String> = fields
        .iter()
        .filter(|f| !matches!(forced(&f.ty), Type::Option(_)))
        .map(|f| format!("\"{}\"", f.name))
        .collect();
    let req = if required.is_empty() {
        String::new()
    } else {
        format!(",\"required\":[{}]", required.join(","))
    };
    format!(
        "{{\"type\":\"object\",\"properties\":{{{}}}{}}}",
        props.join(","),
        req
    )
}

/// Collect JSON Schema numeric constraints from a `where` predicate, returning
/// whether the predicate was captured *in full*. Recognizes `value >=/>/<=/< N`
/// (→ `minimum`/`maximum`/`exclusive*`), `value % K == 0` (→ `multipleOf`),
/// `value != N` (→ `not`/`const`), and `&&` conjunctions; `N`/`K` may be integer
/// or float literals. A disjunction or any other form leaves `false` (the caller
/// then documents the true predicate in a `$comment`).
fn collect_constraints(pred: &Expr, out: &mut Vec<(String, String)>) -> bool {
    let Expr::Binary { op, lhs, rhs, .. } = pred else {
        return false;
    };
    match op {
        // Both sides must be captured for the conjunction to be complete.
        BinOp::And => {
            let a = collect_constraints(lhs, out);
            let b = collect_constraints(rhs, out);
            a && b
        }
        // `value % K == 0` → multipleOf: K (any other `==` is not a keyword).
        BinOp::Eq => {
            if let Expr::Binary {
                op: BinOp::Rem,
                lhs: base,
                rhs: k,
                ..
            } = &**lhs
            {
                if is_value(base) && is_zero(rhs) {
                    if let Some(kv) = num_lit(k) {
                        out.push(("multipleOf".to_string(), kv));
                        return true;
                    }
                }
            }
            false
        }
        // `value != N` → not: { const: N } (a faithful JSON Schema encoding).
        BinOp::NotEq => {
            let lit = match (&**lhs, &**rhs) {
                (l, r) if is_value(l) => num_lit(r),
                (l, r) if is_value(r) => num_lit(l),
                _ => None,
            };
            match lit {
                Some(n) => {
                    out.push(("not".to_string(), format!("{{\"const\":{n}}}")));
                    true
                }
                None => false,
            }
        }
        // `value OP N` or `N OP value` → a bound keyword.
        BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
            let (norm, lit) = match (&**lhs, &**rhs) {
                (l, r) if is_value(l) => (*op, num_lit(r)),
                (l, r) if is_value(r) => (flip(*op), num_lit(l)),
                _ => (*op, None),
            };
            match (norm, lit) {
                (BinOp::GtEq, Some(n)) => push_true(out, "minimum", n),
                (BinOp::Gt, Some(n)) => push_true(out, "exclusiveMinimum", n),
                (BinOp::LtEq, Some(n)) => push_true(out, "maximum", n),
                (BinOp::Lt, Some(n)) => push_true(out, "exclusiveMaximum", n),
                _ => false,
            }
        }
        // Disjunction and everything else can't be a flat keyword set.
        _ => false,
    }
}

/// Push a `(key, value)` constraint and report `true` (a captured atom).
fn push_true(out: &mut Vec<(String, String)>, key: &str, val: String) -> bool {
    out.push((key.to_string(), val));
    true
}

/// True if `e` is the `value` placeholder used in a `where` predicate.
fn is_value(e: &Expr) -> bool {
    matches!(e, Expr::Var { name, .. } if name == "value")
}

/// True if `e` is a literal zero (`0` or `0.0`).
fn is_zero(e: &Expr) -> bool {
    matches!(e, Expr::Int(0)) || matches!(e, Expr::Float(f) if *f == 0.0)
}

/// A numeric literal rendered as a JSON number, or `None` if `e` is not one. A
/// negative literal parses as `Unary(Neg, literal)`, so unwrap one negation.
fn num_lit(e: &Expr) -> Option<String> {
    match e {
        Expr::Int(n) => Some(n.to_string()),
        // `{}` gives the shortest round-tripping form; bounds are always finite
        // (JSON has no NaN/Infinity), so this is always valid JSON.
        Expr::Float(f) => Some(format!("{f}")),
        Expr::Unary {
            op: UnOp::Neg,
            expr,
            ..
        } => match &**expr {
            Expr::Int(n) => Some((-n).to_string()),
            Expr::Float(f) => Some(format!("{}", -f)),
            _ => None,
        },
        _ => None,
    }
}

/// Visit `ty` and every type inside it. Beside [`substitute`] because it walks
/// the same shape — anything the substitution recurses into, this visits.
pub fn walk_type(ty: &Type, f: &mut impl FnMut(&Type)) {
    f(ty);
    match ty {
        Type::Option(a)
        | Type::Array(a)
        | Type::Task(a)
        | Type::Stream(a)
        | Type::Partial(a)
        | Type::ArrayN(a, _)
        | Type::SmallArray(a, _)
        | Type::Omit(a, _)
        | Type::Pick(a, _) => walk_type(a, f),
        Type::Result(a, b) | Type::Merge(a, b) | Type::Map(a, b) => {
            walk_type(a, f);
            walk_type(b, f);
        }
        Type::App(_, args) => {
            for a in args {
                walk_type(a, f);
            }
        }
        Type::Record(fields) => {
            for fl in fields {
                walk_type(&fl.ty, f);
            }
        }
        Type::Enum(variants) => {
            for v in variants {
                for p in &v.payload {
                    walk_type(p, f);
                }
            }
        }
        Type::Fn(params, ret) => {
            for p in params {
                walk_type(p, f);
            }
            walk_type(ret, f);
        }
        _ => {}
    }
}

/// How deeply `ty` nests. `Int64` is 1, `Array<Int64>` is 2,
/// `Array<Array<Int64>>` is 3.
///
/// This is the number monomorphization has to keep bounded (audit A5.2). The
/// language has finitely many type constructors and each takes a fixed number of
/// arguments, so a bounded depth admits finitely many distinct types, and
/// therefore finitely many instantiations of any one function. Polymorphic
/// recursion is the shape that breaks the bound: `f<T>` calling `f<P<T>>` adds
/// one level per call and never repeats a type, so the worklist never empties.
///
/// Beside [`walk_type`] because it walks the same shape.
pub fn type_depth(ty: &Type) -> usize {
    fn deepest(ts: impl Iterator<Item = usize>) -> usize {
        ts.max().unwrap_or(0)
    }
    1 + match ty {
        Type::Option(a)
        | Type::Array(a)
        | Type::Task(a)
        | Type::Stream(a)
        | Type::Partial(a)
        | Type::Lazy(a)
        | Type::ArrayN(a, _)
        | Type::SmallArray(a, _)
        | Type::Omit(a, _)
        | Type::Pick(a, _) => type_depth(a),
        Type::Result(a, b) | Type::Merge(a, b) | Type::Map(a, b) => {
            type_depth(a).max(type_depth(b))
        }
        Type::App(_, args) => deepest(args.iter().map(type_depth)),
        Type::Record(fields) => deepest(fields.iter().map(|f| type_depth(&f.ty))),
        Type::Enum(variants) => deepest(
            variants
                .iter()
                .flat_map(|v| v.payload.iter())
                .map(type_depth),
        ),
        Type::Fn(params, ret) => deepest(params.iter().map(type_depth)).max(type_depth(ret)),
        _ => 0,
    }
}

/// The deepest a type may nest when a generic function is instantiated (audit
/// A5.2, RFC-0016 addendum).
///
/// Monomorphization gives one function one body per distinct instantiation, and
/// polymorphic recursion has no fixed point: `f<T>` calling `f<P<T>>` asks for a
/// new type, and a new body, every turn. The `n <= 0` guard a program writes is a
/// RUN-time test and cannot stop a COMPILE-time worklist. Without a cap the
/// backends ran forever and printed nothing, and `vyrn check` said `ok` about a
/// program `vyrn build` could not finish.
///
/// 64 is far past anything a real generic reaches — the corpus peaks in single
/// digits — and bounding the depth bounds the worklist: finitely many
/// constructors of fixed arity admit finitely many types under a depth bound.
///
/// It lives beside [`type_depth`] rather than in a backend because it bounds a
/// WORKLIST, and since RFC-0101 M1 there are two of those: `vyrn-codegen`
/// re-exports this constant and `vyrn-lower` reads it directly.
pub const MONO_DEPTH_LIMIT: usize = 64;

/// The most parts an instantiated type may have once every named type is
/// expanded ([`expanded_size`]).
///
/// The depth bound alone does not make the compile FINISH, only finite. A record
/// is structural, so `type P<T> = { a: T, b: T }` nested d deep is 2^d leaves and
/// the backends are still asked to lower a billion-member struct at depth 30,
/// long before depth 64. This is the bound that fires first for that shape — at
/// depth 17 for two fields, at depth 9 for four — and the depth bound is what
/// fires first for a spine like `Array<Array<..>>`, which never grows wide.
///
/// 65,536 is roughly a thousand times the largest type the corpus builds.
pub const MONO_SIZE_LIMIT: usize = 65_536;

/// How many parts `ty` has once every named type is expanded, or `None` when
/// that passes `budget`.
///
/// Nesting depth is not the whole bound (audit A5.2). A record is STRUCTURAL
/// here — `type P<T> = { a: T, b: T }` nested d deep expands to 2^d leaves — so
/// a program nesting only 30 levels still asks a backend for a struct with a
/// billion members, and the backend never comes back. This is the number that
/// actually grows, and the walk stops the moment the budget is gone, so asking
/// the question never costs more than the answer.
///
/// A type that names itself (RFC-0096) counts as a leaf where it comes back, so
/// it is measured rather than chased. "Itself" is the name AND the same
/// [`type_depth`]: `Node` inside `Node` is the same type and stops the walk,
/// while `P<X>` inside `P<P<X>>` is a different one and does not — which is the
/// whole shape this measurement exists to catch.
pub fn expanded_size(ty: &Type, types: &HashMap<String, TypeDecl>, budget: usize) -> Option<usize> {
    let mut n = 0usize;
    let mut seen: Vec<(String, usize)> = Vec::new();
    if size_go(ty, types, budget, &mut n, 0, &mut seen) {
        Some(n)
    } else {
        None
    }
}

fn size_go(
    ty: &Type,
    types: &HashMap<String, TypeDecl>,
    budget: usize,
    n: &mut usize,
    depth: usize,
    seen: &mut Vec<(String, usize)>,
) -> bool {
    *n += 1;
    // The node budget alone terminates the walk; the depth bound keeps it off the
    // Rust stack while it gets there.
    if *n > budget || depth > 1024 {
        return false;
    }
    let here = match ty {
        Type::Named(s) | Type::App(s, _) => Some((s.clone(), type_depth(ty))),
        _ => None,
    };
    if let Some(k) = &here {
        if seen.contains(k) {
            return true;
        }
        seen.push(k.clone());
    }
    let resolved;
    let t = match ty {
        Type::Named(_)
        | Type::App(..)
        | Type::Omit(..)
        | Type::Pick(..)
        | Type::Merge(..)
        | Type::Partial(_) => {
            resolved = resolve(ty, types);
            &resolved
        }
        other => other,
    };
    let d = depth + 1;
    let ok = match t {
        Type::Option(a)
        | Type::Array(a)
        | Type::Task(a)
        | Type::Stream(a)
        | Type::Partial(a)
        | Type::Lazy(a)
        | Type::ArrayN(a, _)
        | Type::SmallArray(a, _)
        | Type::Omit(a, _)
        | Type::Pick(a, _) => size_go(a, types, budget, n, d, seen),
        Type::Result(a, b) | Type::Merge(a, b) | Type::Map(a, b) => {
            size_go(a, types, budget, n, d, seen) && size_go(b, types, budget, n, d, seen)
        }
        Type::App(_, args) => args.iter().all(|a| size_go(a, types, budget, n, d, seen)),
        Type::Record(fields) => fields
            .iter()
            .all(|f| size_go(&f.ty, types, budget, n, d, seen)),
        Type::Enum(variants) => variants
            .iter()
            .flat_map(|v| v.payload.iter())
            .all(|p| size_go(p, types, budget, n, d, seen)),
        Type::Fn(params, ret) => {
            params.iter().all(|p| size_go(p, types, budget, n, d, seen))
                && size_go(ret, types, budget, n, d, seen)
        }
        _ => true,
    };
    if here.is_some() {
        seen.pop();
    }
    ok
}

/// Whether `ty` mentions a type parameter anywhere inside it.
pub fn mentions_param(ty: &Type) -> bool {
    let mut found = false;
    walk_type(ty, &mut |t| {
        if matches!(t, Type::Param(_)) {
            found = true;
        }
    });
    found
}

/// Replace generic parameters in `ty` with their bindings from `subst`,
/// recursing through every compound type.
pub fn substitute(ty: &Type, subst: &HashMap<String, Type>) -> Type {
    match ty {
        Type::Param(t) => subst.get(t).cloned().unwrap_or_else(|| ty.clone()),
        Type::Option(inner) => Type::Option(Box::new(substitute(inner, subst))),
        Type::Result(a, b) => Type::Result(
            Box::new(substitute(a, subst)),
            Box::new(substitute(b, subst)),
        ),
        Type::App(name, args) => Type::App(
            name.clone(),
            args.iter().map(|a| substitute(a, subst)).collect(),
        ),
        Type::Record(fields) => Type::Record(
            fields
                .iter()
                .map(|f| Field {
                    name: f.name.clone(),
                    ty: substitute(&f.ty, subst),
                })
                .collect(),
        ),
        Type::Enum(vs) => Type::Enum(
            vs.iter()
                .map(|v| EnumVariant {
                    name: v.name.clone(),
                    payload: v.payload.iter().map(|p| substitute(p, subst)).collect(),
                })
                .collect(),
        ),
        Type::Omit(b, k) => Type::Omit(Box::new(substitute(b, subst)), k.clone()),
        Type::Pick(b, k) => Type::Pick(Box::new(substitute(b, subst)), k.clone()),
        Type::Merge(a, b) => Type::Merge(
            Box::new(substitute(a, subst)),
            Box::new(substitute(b, subst)),
        ),
        Type::Partial(b) => Type::Partial(Box::new(substitute(b, subst))),
        Type::Array(inner) => Type::Array(Box::new(substitute(inner, subst))),
        Type::ArrayN(inner, n) => Type::ArrayN(Box::new(substitute(inner, subst)), *n),
        Type::SmallArray(inner, n) => Type::SmallArray(Box::new(substitute(inner, subst)), *n),
        Type::Map(k, v) => Type::Map(
            Box::new(substitute(k, subst)),
            Box::new(substitute(v, subst)),
        ),
        Type::Task(inner) => Type::Task(Box::new(substitute(inner, subst))),
        // A `Stream<T>` (RFC-0075) substitutes like the `Array<T>` it lowers to.
        // M1 never needed this because `fromArray` was the only producer and it
        // was always called at a concrete element type; M2's `map<T, U>(s:
        // Stream<T>, ..) -> Stream<U>` is the first signature with a type
        // parameter *inside* a stream, and without this arm `Stream<U>` reached
        // codegen as a bare parameter.
        Type::Stream(inner) => Type::Stream(Box::new(substitute(inner, subst))),
        // A function-value type (RFC-0023): substitute into its parameter and
        // return types so a generic `fn(T) -> U` monomorphizes with the rest.
        Type::Fn(params, ret) => Type::Fn(
            params.iter().map(|p| substitute(p, subst)).collect(),
            Box::new(substitute(ret, subst)),
        ),
        // A `lazy T` field (RFC-0085 M4a) substitutes into what it defers, so
        // `type Box<T> = { v: lazy T }` monomorphizes with everything else.
        Type::Lazy(inner) => Type::Lazy(Box::new(substitute(inner, subst))),
        other => other.clone(),
    }
}

/// Reduce `ty` to its structural form (scalar, `Record`, `Option`, `Result`, …).
/// A program's type declarations by name — the map `resolve` and the JSON codec
/// take. Every engine builds one of these; this is the shared spelling.
pub fn decl_map(p: &crate::ast::Program) -> HashMap<String, TypeDecl> {
    p.type_decls
        .iter()
        .map(|t| (t.name.clone(), t.clone()))
        .collect()
}

pub fn resolve(ty: &Type, types: &HashMap<String, TypeDecl>) -> Type {
    resolve_d(ty, types, 0)
}

/// The fields of `ty` if it (resolves to) a record; otherwise `None`.
pub fn record_fields(ty: &Type, types: &HashMap<String, TypeDecl>) -> Option<Vec<Field>> {
    match resolve(ty, types) {
        Type::Record(f) => Some(f),
        _ => None,
    }
}

fn resolve_d(ty: &Type, types: &HashMap<String, TypeDecl>, depth: usize) -> Type {
    if depth > MAX_DEPTH {
        return Type::Unit;
    }
    match ty {
        // `Code` (RFC-0054) is a builtin *opaque* type with no declaration — it
        // resolves to itself, not to `Unit` (the unknown-named fallback). A user
        // `type Code` (if any) wins, so pre-existing programs are unaffected.
        Type::Named(n) if n == "Code" && !types.contains_key("Code") => {
            Type::Named("Code".to_string())
        }
        // `Token` (RFC-0054) is the builtin record `lex()` returns — a magic type
        // (not an injected decl, so it never collides with a user `type Token`,
        // which wins). It resolves to its record shape so `.kind`/`.text`/… work.
        Type::Named(n) if n == "Token" && !types.contains_key("Token") => Type::Record(vec![
            Field {
                name: "kind".to_string(),
                ty: Type::Str,
            },
            Field {
                name: "text".to_string(),
                ty: Type::Str,
            },
            Field {
                name: "line".to_string(),
                ty: Type::Int,
            },
            Field {
                name: "col".to_string(),
                ty: Type::Int,
            },
        ]),
        Type::Named(n) => match types.get(n) {
            Some(d) => resolve_d(&d.base, types, depth + 1),
            None => Type::Unit,
        },
        // A generic application: substitute the declaration's parameters, then
        // resolve the result.
        Type::App(name, args) => match types.get(name) {
            Some(d) if d.type_params.len() == args.len() => {
                let s: HashMap<String, Type> = d
                    .type_params
                    .iter()
                    .cloned()
                    .zip(args.iter().cloned())
                    .collect();
                let based = substitute(&d.base, &s);
                resolve_d(&based, types, depth + 1)
            }
            _ => Type::Unit,
        },
        Type::Omit(base, keys) => match fields_d(base, types, depth) {
            Some(fs) => Type::Record(fs.into_iter().filter(|f| !keys.contains(&f.name)).collect()),
            None => Type::Unit,
        },
        Type::Pick(base, keys) => match fields_d(base, types, depth) {
            Some(fs) => Type::Record(fs.into_iter().filter(|f| keys.contains(&f.name)).collect()),
            None => Type::Unit,
        },
        Type::Merge(a, b) => match (fields_d(a, types, depth), fields_d(b, types, depth)) {
            (Some(fa), Some(fb)) => Type::Record(merge_fields(fa, fb)),
            _ => Type::Unit,
        },
        // `Partial<T>` — every field becomes Option<field>.
        Type::Partial(base) => match fields_d(base, types, depth) {
            Some(fs) => Type::Record(
                fs.into_iter()
                    .map(|f| Field {
                        name: f.name,
                        ty: Type::Option(Box::new(f.ty)),
                    })
                    .collect(),
            ),
            None => Type::Unit,
        },
        // RFC-0085 M4a: `lazy T` IS a stored nullary closure, so resolving one
        // answers the representation. This ONE arm is why the feature needs no
        // layout, ownership, movecheck or dispatcher work of its own — every
        // consumer that resolves a type sees the `fn() -> T` RFC-0037 already
        // built. It does NOT reach a record's fields (this function does not
        // recurse into `Type::Record`), so `Field.ty` keeps the raw marker for
        // the three places that must see it: the field READ (which forces), the
        // codec (which forces too) and reflection.
        Type::Lazy(inner) => Type::Fn(Vec::new(), Box::new(resolve_d(inner, types, depth + 1))),
        other => other.clone(),
    }
}

/// What a `lazy T` field DEFERS, or `None` for an ordinary field (RFC-0085 M4a).
///
/// The one spelling of "is this field forced when read", so the checker, the
/// interpreter, both backends and the codec cannot disagree about it.
pub fn deferred(ty: &Type) -> Option<&Type> {
    match ty {
        Type::Lazy(inner) => Some(inner),
        _ => None,
    }
}

/// A field's type as a *value* sees it: `lazy T` becomes `T`, everything else is
/// itself. What a read of the field yields, and what the codec encodes.
pub fn forced(ty: &Type) -> Type {
    deferred(ty).cloned().unwrap_or_else(|| ty.clone())
}

fn fields_d(ty: &Type, types: &HashMap<String, TypeDecl>, depth: usize) -> Option<Vec<Field>> {
    match resolve_d(ty, types, depth + 1) {
        Type::Record(f) => Some(f),
        _ => None,
    }
}

/// Combine two field lists: `a`'s order first, `b` overriding on name conflict,
/// then `b`'s new fields appended.
fn merge_fields(fa: Vec<Field>, fb: Vec<Field>) -> Vec<Field> {
    let mut out: Vec<Field> = Vec::new();
    for f in fa {
        match fb.iter().find(|x| x.name == f.name) {
            Some(bf) => out.push(bf.clone()),
            None => out.push(f),
        }
    }
    for f in fb {
        if !out.iter().any(|x| x.name == f.name) {
            out.push(f);
        }
    }
    out
}

/// What a `where` predicate has in scope: a record base binds every field by name
/// (a cross-field predicate names them), and every other base binds the whole value
/// as `value`. `Some(i)` is the field's index in the base record.
///
/// The predicate itself cannot be lowered in one place — one backend prints LLVM
/// text, the other wasm bytes, and the interpreter evaluates it — so what is shared
/// is the STRUCTURE all of them walk. `vyrn-codegen` had three copies before
/// RFC-0077 M2d wanted a fourth, and RFC-0078 M3 moved it HERE because the decode
/// path needs it too: a refined type's decoder calls a synthesized `Bool`-returning
/// function whose parameters are exactly this list, so the accumulating `validate`
/// check and the trapping one cannot disagree about what `value` binds.
pub fn predicate_binds(decl: &TypeDecl) -> Vec<(String, Type, Option<usize>)> {
    match &decl.base {
        Type::Record(fs) => fs
            .iter()
            .enumerate()
            .map(|(i, f)| (f.name.clone(), f.ty.clone(), Some(i)))
            .collect(),
        base => vec![("value".to_string(), base.clone(), None)],
    }
}

#[cfg(test)]
mod struct_key_tests {
    use super::*;

    /// The structural identity of a fixed set of types, as bytes.
    ///
    /// This is the pin [`struct_key`]'s argument needs and did not have. The key
    /// is `sha256(format!("{ty:?}"))[..16]`, and `Debug` is a debugging aid, not
    /// a format anyone promised: a field rename, a changed derive or a different
    /// rendering moves every key at once. Nothing in a single process can notice
    /// that — a test that computes the key twice agrees with itself whatever the
    /// key is — so the expected values are WRITTEN DOWN here, and a build that
    /// renders `Type` differently from the build that wrote them fails naming the
    /// type that moved.
    ///
    /// What to do when it fails: the change is safe as long as no artifact
    /// outside an emitted module carries one of these names — the condition
    /// stated on [`struct_key`] — in which case update the row. If that
    /// condition has stopped holding, the row is the compatibility break, and
    /// updating it is the wrong move.
    ///
    /// The rows are one per shape the identity has to tell apart: the two
    /// integer families (`Int64` is a distinct variant from `Int8`), a name, a
    /// record whose fields differ only by name and only by type, a payload enum,
    /// and the two-argument constructors.
    const PINNED: &[(&str, &str)] = &[
        ("Int64", "0b5f608070c6ce3b"),
        ("Int8", "2682e2651c00bb25"),
        ("UInt8", "6b60ff17449c2bfe"),
        ("String", "8084a51b3c649e88"),
        ("Bool", "49f411f0a1a7f719"),
        ("R", "6dc9d47663615470"),
        ("Option<Int64>", "3cca7115ecddc468"),
        ("Array<Int64>", "d9c366f005660b4f"),
        ("Array<Int64, 4>", "789ccf35c7706c73"),
        ("{ a: Int64 }", "9410e46ed45e2516"),
        ("{ b: Int64 }", "b1e8f1e6073bf44e"),
        ("{ a: String }", "71a7ba0548c213ff"),
        ("enum { A }", "4312633fc2f748cb"),
        ("Result<Int64, String>", "2a121af6953f575a"),
        ("Map<String, Int64>", "4986ea0126622a62"),
        ("Box<Int64>", "9969edc012fba3aa"),
    ];

    fn pinned_types() -> Vec<(&'static str, Type)> {
        let b = |t: Type| Box::new(t);
        vec![
            ("Int64", Type::Int),
            (
                "Int8",
                Type::IntN {
                    bits: 8,
                    signed: true,
                },
            ),
            (
                "UInt8",
                Type::IntN {
                    bits: 8,
                    signed: false,
                },
            ),
            ("String", Type::Str),
            ("Bool", Type::Bool),
            ("R", Type::Named("R".into())),
            ("Option<Int64>", Type::Option(b(Type::Int))),
            ("Array<Int64>", Type::Array(b(Type::Int))),
            ("Array<Int64, 4>", Type::ArrayN(b(Type::Int), 4)),
            (
                "{ a: Int64 }",
                Type::Record(vec![Field {
                    name: "a".into(),
                    ty: Type::Int,
                }]),
            ),
            (
                "{ b: Int64 }",
                Type::Record(vec![Field {
                    name: "b".into(),
                    ty: Type::Int,
                }]),
            ),
            (
                "{ a: String }",
                Type::Record(vec![Field {
                    name: "a".into(),
                    ty: Type::Str,
                }]),
            ),
            (
                "enum { A }",
                Type::Enum(vec![EnumVariant {
                    name: "A".into(),
                    payload: vec![Type::Int],
                }]),
            ),
            (
                "Result<Int64, String>",
                Type::Result(b(Type::Int), b(Type::Str)),
            ),
            ("Map<String, Int64>", Type::Map(b(Type::Str), b(Type::Int))),
            ("Box<Int64>", Type::App("Box".into(), vec![Type::Int])),
        ]
    }

    /// The same type produces the same identity in a LATER BUILD, not merely
    /// twice in one run. See [`PINNED`].
    #[test]
    fn struct_key_is_pinned() {
        let rows = pinned_types();
        assert_eq!(rows.len(), PINNED.len(), "a pinned row lost its type");
        for ((label, ty), (plabel, want)) in rows.iter().zip(PINNED) {
            assert_eq!(label, plabel, "the two lists drifted apart");
            assert_eq!(
                &struct_key(ty),
                want,
                "the structural identity of `{label}` moved: `Debug` for Type \
                 renders differently than it did when this row was written, so \
                 every synthesized symbol keyed on a type has been renamed"
            );
        }
    }

    /// No two distinct types share one identity.
    ///
    /// The claim [`struct_key`] exists for, checked over generated trees rather
    /// than the handful of pairs anyone thought to write down — including the
    /// pairs the READABLE mangle collapses, which is the collision class this
    /// mechanism replaced (RFC-0077 M2e). `vyrn-codegen` checks the same claim
    /// through its symbols; this checks the function itself, so the JSON codec's
    /// two names are covered by the same proof.
    #[test]
    fn struct_key_is_injective_over_generated_types() {
        let f = |n: &str, t: Type| Field {
            name: n.to_string(),
            ty: t,
        };
        let mut seeds = vec![
            Type::Int,
            Type::IntN {
                bits: 8,
                signed: true,
            },
            Type::IntN {
                bits: 8,
                signed: false,
            },
            Type::IntN {
                bits: 64,
                signed: true,
            },
            Type::Float,
            Type::Float32,
            Type::Bool,
            Type::Str,
            Type::Unit,
            // The pairs a readable mangle collapses: `Option<Int64>` and a user
            // type spelled `OptInt64`, a record and the three characters `Rec`,
            // every transformer and `Xf`.
            Type::Named("OptInt64".into()),
            Type::Named("Rec".into()),
            Type::Named("Xf".into()),
            Type::Named("R".into()),
            Type::Param("R".into()),
            Type::Record(vec![]),
            Type::Record(vec![f("a", Type::Int)]),
            Type::Record(vec![f("b", Type::Int)]),
            Type::Record(vec![f("a", Type::Str)]),
            Type::Record(vec![f("a", Type::Int), f("b", Type::Int)]),
            Type::Enum(vec![]),
            Type::Enum(vec![EnumVariant {
                name: "A".into(),
                payload: vec![],
            }]),
            Type::Enum(vec![EnumVariant {
                name: "A".into(),
                payload: vec![Type::Int],
            }]),
            Type::Omit(Box::new(Type::Named("R".into())), vec!["a".into()]),
            Type::Omit(Box::new(Type::Named("R".into())), vec!["b".into()]),
            Type::Pick(Box::new(Type::Named("R".into())), vec!["a".into()]),
            Type::Partial(Box::new(Type::Named("R".into()))),
            Type::Logger,
            Type::Never,
            Type::Err,
        ];
        // The hazard is real before it is ruled out: these two DO collide under
        // the readable mangle, so a generator that missed them would prove
        // nothing.
        assert_eq!(
            Type::Option(Box::new(Type::Int)).to_string(),
            "Option<Int64>"
        );

        for round in 0..2 {
            let base: Vec<Type> = if round == 0 {
                seeds.clone()
            } else {
                seeds[..320].to_vec()
            };
            let pairs: Vec<Type> = base[..8.min(base.len())].to_vec();
            for t in &base {
                let b = || Box::new(t.clone());
                seeds.extend([
                    Type::Option(b()),
                    Type::Array(b()),
                    Type::ArrayN(b(), 4),
                    Type::ArrayN(b(), 8),
                    Type::SmallArray(b(), 4),
                    Type::Stream(b()),
                    Type::Task(b()),
                    Type::Lazy(b()),
                    Type::App("P".into(), vec![t.clone()]),
                    Type::App("Q".into(), vec![t.clone()]),
                    Type::Fn(vec![], b()),
                    Type::Fn(vec![t.clone()], Box::new(Type::Unit)),
                    Type::Record(vec![f("a", t.clone())]),
                    Type::Enum(vec![EnumVariant {
                        name: "V".into(),
                        payload: vec![t.clone()],
                    }]),
                ]);
            }
            for a in &pairs {
                for c in &pairs {
                    seeds.extend([
                        Type::Result(Box::new(a.clone()), Box::new(c.clone())),
                        Type::Map(Box::new(a.clone()), Box::new(c.clone())),
                        Type::App("P".into(), vec![a.clone(), c.clone()]),
                        Type::Fn(vec![a.clone(), c.clone()], Box::new(Type::Unit)),
                    ]);
                }
            }
        }

        let mut seen: HashMap<String, Type> = HashMap::new();
        for ty in &seeds {
            let k = struct_key(ty);
            if let Some(prev) = seen.insert(k.clone(), ty.clone()) {
                assert_eq!(
                    &prev, ty,
                    "two distinct types share the identity `{k}`: every symbol \
                     keyed on it emits one body and routes both types through it"
                );
            }
        }
        assert!(
            seeds.len() > 5_000,
            "only {} types generated; the coverage shrank",
            seeds.len()
        );
    }

    /// The two synthesized JSON names carry the identity, so they are injective
    /// wherever it is — and they are distinct from each other for one type, which
    /// the shared key alone does not say.
    #[test]
    fn the_json_codec_names_are_the_shared_identity() {
        let ty = Type::Record(vec![Field {
            name: "a".into(),
            ty: Type::Int,
        }]);
        let k = struct_key(&ty);
        assert!(crate::jsonenc::enc_name(&ty).ends_with(&k));
        assert!(crate::jsondec::top_name(&ty).ends_with(&k));
        assert_ne!(crate::jsonenc::enc_name(&ty), crate::jsondec::top_name(&ty));
    }
}

#[cfg(test)]
mod json_schema_tests {
    use super::*;

    /// Parse `src`, return the JSON Schema for the named type. Both the interpreter
    /// and codegen call `json_schema_string` with the same inputs, so asserting on
    /// it here pins the exact bytes both backends emit.
    fn schema_of(src: &str, name: &str) -> String {
        let toks = crate::lexer::lex(src).expect("lex");
        let prog = crate::parser::parse(toks).expect("parse");
        let types: HashMap<String, TypeDecl> = prog
            .type_decls
            .iter()
            .map(|t| (t.name.clone(), t.clone()))
            .collect();
        json_schema_string(&types[name], &types)
    }

    #[test]
    fn integer_minimum() {
        assert_eq!(
            schema_of("type Age = Int64 where value >= 18", "Age"),
            "{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\"type\":\"integer\",\"minimum\":18}"
        );
    }

    #[test]
    fn integer_min_and_max() {
        assert_eq!(
            schema_of("type Port = Int64 where value >= 1 && value <= 65535", "Port"),
            "{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\"type\":\"integer\",\"minimum\":1,\"maximum\":65535}"
        );
    }

    #[test]
    fn exclusive_bounds_and_multiple_of() {
        assert_eq!(
            schema_of("type Even = Int64 where value % 2 == 0", "Even"),
            "{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\"type\":\"integer\",\"multipleOf\":2}"
        );
        assert_eq!(
            schema_of("type Big = Int64 where value > 100", "Big"),
            "{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\"type\":\"integer\",\"exclusiveMinimum\":100}"
        );
    }

    #[test]
    fn float_number_with_bounds() {
        assert_eq!(
            schema_of("type Ratio = Float64 where value > 0.0 && value <= 1.0", "Ratio"),
            "{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\"type\":\"number\",\"exclusiveMinimum\":0,\"maximum\":1}"
        );
    }

    #[test]
    fn negative_bound_is_captured() {
        // `-273.15` parses as Unary(Neg, Float); `num_lit` unwraps the negation.
        assert_eq!(
            schema_of("type Temp = Float64 where value >= -273.15", "Temp"),
            "{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\"type\":\"number\",\"minimum\":-273.15}"
        );
    }

    #[test]
    fn negative_integer_bound_is_captured() {
        // `value >= -5` parses as Unary(Neg, Int); `predicate_bounds` unwraps
        // the negation like `num_lit` does for floats — the old reflection saw
        // no literal and reported no minimum at all.
        assert_eq!(
            schema_of("type Debt = Int64 where value >= -5 && value <= 5", "Debt"),
            "{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\"type\":\"integer\",\"minimum\":-5,\"maximum\":5}"
        );
    }

    #[test]
    fn record_object_with_required() {
        // A named validated field becomes a `$ref` into `$defs` (the schema
        // keeps the user's name); an Option field is optional.
        assert_eq!(
            schema_of(
                "type Age = Int64 where value >= 18 \
                 type User = { name: String, age: Age, nick: Option<String> }",
                "User"
            ),
            "{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\"type\":\"object\",\
             \"properties\":{\"name\":{\"type\":\"string\"},\"age\":{\"$ref\":\"#/$defs/Age\"},\
             \"nick\":{\"type\":\"string\"}},\"required\":[\"name\",\"age\"],\
             \"$defs\":{\"Age\":{\"type\":\"integer\",\"minimum\":18}}}"
        );
    }

    #[test]
    fn array_field_uses_items() {
        assert_eq!(
            schema_of("type Bag = { tags: Array<String> }", "Bag"),
            "{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\"type\":\"object\",\
             \"properties\":{\"tags\":{\"type\":\"array\",\"items\":{\"type\":\"string\"}}},\"required\":[\"tags\"]}"
        );
    }

    #[test]
    fn string_length_maps_to_min_max_length() {
        assert_eq!(
            schema_of("type Username = String where value.byteLength >= 3 && value.byteLength <= 16", "Username"),
            "{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\"type\":\"string\",\"minLength\":3,\"maxLength\":16}"
        );
    }

    #[test]
    fn string_exclusive_length_floors_to_inclusive() {
        // `value.byteLength > 2` ⇒ minLength 3 (JSON Schema minLength is inclusive).
        assert_eq!(
            schema_of("type S = String where value.byteLength > 2", "S"),
            "{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\"type\":\"string\",\"minLength\":3}"
        );
    }

    #[test]
    fn exclusive_length_bound_at_the_i64_edge_saturates() {
        // The step to an inclusive bound saturates at the `i64` edges, like
        // `predicate_bounds`' numeric step — an extreme literal must not
        // overflow the reflection.
        assert_eq!(
            schema_of(
                "type Edge = String where value.byteLength > 9223372036854775807",
                "Edge"
            ),
            "{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\"type\":\"string\",\"minLength\":9223372036854775807}"
        );
    }

    #[test]
    fn not_equal_maps_to_not_const() {
        // A multi-clause predicate is captured faithfully: `!= N` → not/const.
        assert_eq!(
            schema_of(
                "type Score = Int64 where value > 0 && value % 2 == 0 && value != 100",
                "Score"
            ),
            "{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\"type\":\"integer\",\
             \"exclusiveMinimum\":0,\"multipleOf\":2,\"not\":{\"const\":100}}"
        );
    }

    #[test]
    fn disjunction_is_documented_not_dropped() {
        // A predicate the keyword model can't encode keeps a faithful `$comment`
        // rather than silently under-specifying.
        assert_eq!(
            schema_of(
                "type Small = Int64 where value < 10 || value > 1000",
                "Small"
            ),
            "{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\"type\":\"integer\",\
             \"$comment\":\"constrained by: value < 10 || value > 1000\"}"
        );
    }

    #[test]
    fn partial_capture_keeps_mapped_parts_and_comments() {
        // `value >= 0` maps; the `!= 7` after an OR makes the whole thing partial,
        // so the mapped bound stays AND the full predicate is documented.
        let s = schema_of(
            "type T = Int64 where value >= 0 && (value < 3 || value > 5)",
            "T",
        );
        assert!(s.contains("\"minimum\":0"), "keeps mapped bound: {s}");
        assert!(
            s.contains("\"$comment\":\"constrained by:"),
            "documents remainder: {s}"
        );
    }

    #[test]
    fn regex_maps_to_anchored_pattern() {
        // `=~` reflects to an anchored JSON Schema `pattern` (backslashes escaped).
        let s = schema_of("type Slug = String where value =~ \"[a-z]+\"", "Slug");
        assert!(
            s.contains("\"pattern\":\"^[a-z]+$\""),
            "anchored pattern: {s}"
        );
    }

    /// A schema is a JSON document, so this crate's own strict reader must read
    /// back everything `jsonSchema` emits — including a `where` pattern carrying
    /// control bytes, which RFC 8259 forbids raw inside a string. A second,
    /// incomplete escaper on this path wrote a raw `\r` and the reader refused
    /// the document it had just produced.
    #[test]
    fn a_hostile_pattern_round_trips_through_the_strict_reader() {
        for (src, name) in [
            ("type T = String where value =~ \"a\\rb\"", "T"),
            ("type T = String where value =~ \"a\\tb\\nc\"", "T"),
            // `$comment` carries the whole predicate when the keywords cannot.
            (
                "type T = String where value =~ \"a\\rb\" || value.byteLength > 2",
                "T",
            ),
        ] {
            let s = schema_of(src, name);
            assert!(
                !s.bytes().any(|b| b < 0x20),
                "raw control byte in a JSON string ({src}): {s:?}"
            );
            crate::codec::parse(&s).unwrap_or_else(|e| {
                panic!("strict reader refused its own output ({src}): {e:?}\n{s}")
            });
        }
    }

    #[test]
    fn multiple_patterns_combine_with_allof() {
        // Size + two regex clauses: length maps directly, the patterns go in `allOf`
        // (a JSON object permits only one `pattern`).
        let s = schema_of(
            "type W = String where value.byteLength >= 4 && value =~ \"[a-z]+\" && value =~ \"(.a)*\"",
            "W",
        );
        assert!(s.contains("\"minLength\":4"), "length maps: {s}");
        assert!(
            s.contains("\"allOf\":[{\"pattern\":\"^[a-z]+$\"},{\"pattern\":\"^(.a)*$\"}]"),
            "patterns combined via allOf: {s}"
        );
        // Exactly one `pattern` key would be a duplicate → must not appear bare.
        assert!(
            !s.contains("$\",\"pattern\""),
            "no duplicate pattern key: {s}"
        );
    }

    #[test]
    fn recursive_record_terminates_with_root_ref() {
        // A self-referential record must not expand forever (this used to
        // stack-overflow the compiler); the back-edge is a real `$ref` to the
        // document root — a faithful recursive schema, not a lossy comment.
        let s = schema_of("type Node = { name: String, next: Option<Node> }", "Node");
        assert!(s.contains("\"next\":{\"$ref\":\"#\"}"), "{s}");
        assert!(s.contains("\"name\":{\"type\":\"string\"}"), "{s}");
    }

    #[test]
    fn mutually_recursive_records_terminate() {
        // A → B via `$defs`; B's back-edge to the root A is `{"$ref":"#"}`.
        let s = schema_of(
            "type A = { b: Option<B> } \
             type B = { a: Option<A> }",
            "A",
        );
        assert!(s.contains("\"b\":{\"$ref\":\"#/$defs/B\"}"), "{s}");
        assert!(
            s.contains(
                "\"$defs\":{\"B\":{\"type\":\"object\",\"properties\":{\"a\":{\"$ref\":\"#\"}}}}"
            ),
            "{s}"
        );
    }

    #[test]
    fn repeated_reference_shares_one_def() {
        // The same named type on sibling paths renders one `$defs` entry and
        // two `$ref`s — no duplication.
        let s = schema_of(
            "type Age = Int64 where value >= 18 \
             type Pair = { x: Age, y: Age }",
            "Pair",
        );
        assert_eq!(s.matches("{\"$ref\":\"#/$defs/Age\"}").count(), 2, "{s}");
        assert_eq!(s.matches("\"minimum\":18").count(), 1, "{s}");
    }

    #[test]
    fn inline_field_refinements_reach_the_schema() {
        // Zod-style inline `where` on fields lands in the field's schema, and
        // the record-level cross-field `where` keeps its `$comment`.
        let s = schema_of(
            "type User = { name: String where value.byteLength >= 3, \
                           age: Int64 where value >= 18 } where age < 150",
            "User",
        );
        assert!(
            s.contains("\"name\":{\"type\":\"string\",\"minLength\":3}"),
            "{s}"
        );
        assert!(
            s.contains("\"age\":{\"type\":\"integer\",\"minimum\":18}"),
            "{s}"
        );
        assert!(
            s.contains("\"$comment\":\"constrained by: age < 150\""),
            "{s}"
        );
    }

    #[test]
    fn cross_field_record_documents_invariant() {
        let s = schema_of("type R = { a: Int64, b: Int64 } where a < b", "R");
        assert!(s.contains("\"type\":\"object\""), "still an object: {s}");
        assert!(
            s.contains("\"required\":[\"a\",\"b\"]"),
            "required intact: {s}"
        );
        assert!(
            s.contains("\"$comment\":\"constrained by: a < b\""),
            "documents invariant: {s}"
        );
    }

    #[test]
    fn sized_ints_emit_width_bounds() {
        // A sized int is a deliberate wire-width choice; its bounds are part
        // of the contract. The default Int64 stays a bare "integer".
        assert_eq!(
            schema_of("type Byte = UInt8", "Byte"),
            "{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\
             \"type\":\"integer\",\"minimum\":0,\"maximum\":255}"
        );
        assert_eq!(
            schema_of("type Small = Int16", "Small"),
            "{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\
             \"type\":\"integer\",\"minimum\":-32768,\"maximum\":32767}"
        );
        // UInt64's maximum exceeds an Int64 literal (unimportable) — min only.
        assert_eq!(
            schema_of("type Big = UInt64", "Big"),
            "{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\
             \"type\":\"integer\",\"minimum\":0}"
        );
    }

    #[test]
    fn refined_sized_int_merges_bounds_canonically() {
        // The predicate wins its bound family; the width fills the other.
        // Keys stay in the importer's canonical order (minimum before
        // maximum) even though the width max is not from the predicate.
        assert_eq!(
            schema_of("type Small = UInt8 where value >= 3", "Small"),
            "{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\
             \"type\":\"integer\",\"minimum\":3,\"maximum\":255}"
        );
    }

    #[test]
    fn payloadless_enum_emits_enum_schema() {
        assert_eq!(
            schema_of("type Color = | Red | Green | Blue", "Color"),
            "{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\
             \"enum\":[\"Red\",\"Green\",\"Blue\"]}"
        );
    }

    #[test]
    fn payload_enum_emits_oneof() {
        // RFC-0024: a payload enum is externally tagged, arity-shaped `oneOf`.
        let s = schema_of(
            "type Shape = | Circle(Int64) | Rect(Int64, Int64) | Unit",
            "Shape",
        );
        assert_eq!(
            s,
            "{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\
             \"oneOf\":[\
             {\"type\":\"object\",\"properties\":{\"Circle\":{\"type\":\"integer\"}},\"required\":[\"Circle\"]},\
             {\"type\":\"object\",\"properties\":{\"Rect\":{\"type\":\"array\",\
             \"prefixItems\":[{\"type\":\"integer\"},{\"type\":\"integer\"}],\"items\":false}},\"required\":[\"Rect\"]},\
             {\"const\":\"Unit\"}]}"
        );
    }

    #[test]
    fn result_emits_ok_err_oneof() {
        let s = schema_of("type R = { r: Result<Int64, String> }", "R");
        assert!(
            s.contains(
                "\"oneOf\":[\
                 {\"type\":\"object\",\"properties\":{\"Ok\":{\"type\":\"integer\"}},\"required\":[\"Ok\"]},\
                 {\"type\":\"object\",\"properties\":{\"Err\":{\"type\":\"string\"}},\"required\":[\"Err\"]}]"
            ),
            "{s}"
        );
    }
}

/// Bind type parameters by matching a (possibly generic) parameter type against
/// a concrete argument type. Mirrors the checker's `unify`, minus error checks
/// (the checker already validated the call).
/// The rule is mechanical, and RFC-0086 M1 said so when it decided this is
/// exhaustiveness rather than a protocol: **same constructor, recurse on the
/// children; different constructors, bind nothing.** Nothing is declared here
/// and no third party could declare it — "how do two type constructors match"
/// has no author but the compiler.
///
/// So the match is over `pty` with **no `_` arm**, and a new [`Type`] variant
/// has to answer. It used to pair the two types and fall through, which meant
/// four constructors — `Record`, `Enum`, `Lazy`, `Task` — walked past a type
/// parameter they contained and left it for `applied_type` to fill with `Unit`.
/// The inner match on `aty` keeps its `_`: that one is the rule's second half.
pub fn solve_param(pty: &Type, aty: &Type, subst: &mut HashMap<String, Type>) {
    match pty {
        Type::Param(t) => {
            subst.entry(t.clone()).or_insert_with(|| aty.clone());
        }
        Type::Option(p) => {
            if let Type::Option(a) = aty {
                solve_param(p, a, subst);
            }
        }
        Type::Result(p1, p2) => {
            if let Type::Result(a1, a2) = aty {
                solve_param(p1, a1, subst);
                solve_param(p2, a2, subst);
            }
        }
        Type::App(pn, pa) => {
            if let Type::App(an, aa) = aty {
                if pn == an && pa.len() == aa.len() {
                    for (p, a) in pa.iter().zip(aa) {
                        solve_param(p, a, subst);
                    }
                }
            }
        }
        // Generic collection/reference element inference (RFC-0023): bind the
        // element type parameter from the concrete argument.
        // A LITERAL is a fixed `[N x T]` here whatever slot it is headed for —
        // the growable and small-buffer shapes are reached by the reshape in
        // `coerce`, after this solve names the element. So a growable or
        // small-buffer parameter binds from a fixed actual too: without it
        // `Deque { front: [2, 1] }` solved nothing, and the reshape then
        // compared `Int64` against the unsolved `T` (whose `llt` is `void`),
        // declined, and handed a `[2 x i64]` to a `{ptr,len,cap}` field.
        Type::Array(p) => match aty {
            Type::Array(a) | Type::ArrayN(a, _) => solve_param(p, a, subst),
            _ => {}
        },
        Type::ArrayN(p, _) => {
            if let Type::ArrayN(a, _) = aty {
                solve_param(p, a, subst);
            }
        }
        Type::SmallArray(p, _) => match aty {
            Type::SmallArray(a, _) | Type::ArrayN(a, _) => solve_param(p, a, subst),
            _ => {}
        },
        // A `Stream<T>` (RFC-0075) binds its element exactly as an `Array<T>`
        // does — it is the same three words. M2's combinators are the first
        // signatures with a type parameter inside a stream, and without this the
        // direct backend refused `take` outright while the LLVM emitter quietly
        // substituted `Unit`, i.e. the two backends specialized different
        // functions for one call site.
        Type::Stream(p) => {
            if let Type::Stream(a) = aty {
                solve_param(p, a, subst);
            }
        }
        Type::Map(pk, pv) => {
            if let Type::Map(ak, av) = aty {
                solve_param(pk, ak, subst);
                solve_param(pv, av, subst);
            }
        }
        // A `fn` type (RFC-0023/RFC-0037), parameter-wise then on the return.
        // Without this a generic record holding a `fn` whose parameter is the
        // record's own type parameter — `Deferred<P, T> = { run: fn(P) -> T }`,
        // the `std/ui` `ParamQuery` shape — solves NOTHING from its field, and
        // `applied_type` fills both in with `Unit`.
        Type::Fn(pp, pr) => {
            if let Type::Fn(ap, ar) = aty {
                if pp.len() == ap.len() {
                    for (p, a) in pp.iter().zip(ap) {
                        solve_param(p, a, subst);
                    }
                    solve_param(pr, ar, subst);
                }
            }
        }
        // A record matches FIELD BY NAME, not by position: width subtyping
        // (RFC-0002) lets the concrete side carry fields the pattern does not
        // ask for, and its order is its own.
        Type::Record(pf) => {
            if let Type::Record(af) = aty {
                for p in pf {
                    if let Some(a) = af.iter().find(|a| a.name == p.name) {
                        solve_param(&p.ty, &a.ty, subst);
                    }
                }
            }
        }
        // An enum matches VARIANT BY NAME, then payload-wise.
        Type::Enum(pv) => {
            if let Type::Enum(av) = aty {
                for p in pv {
                    if let Some(a) = av.iter().find(|a| a.name == p.name) {
                        for (pp, ap) in p.payload.iter().zip(&a.payload) {
                            solve_param(pp, ap, subst);
                        }
                    }
                }
            }
        }
        // `lazy T` IS `fn() -> T` at runtime (RFC-0085 M4a) and
        // `types::resolve` answers that, so the concrete side reaches here in
        // either spelling and both bind the same `T`.
        Type::Lazy(p) => match aty {
            Type::Lazy(a) => solve_param(p, a, subst),
            Type::Fn(ap, ar) if ap.is_empty() => solve_param(p, ar, subst),
            _ => {}
        },
        Type::Task(p) => {
            if let Type::Task(a) = aty {
                solve_param(p, a, subst);
            }
        }
        // The compile-time record transformers (RFC-0002 §7). The checker
        // expands one before codegen sees it, so these arms are the rule
        // written down rather than a path anything takes today.
        Type::Partial(p) => {
            if let Type::Partial(a) = aty {
                solve_param(p, a, subst);
            }
        }
        Type::Omit(p, pk) => {
            if let Type::Omit(a, ak) = aty {
                if pk == ak {
                    solve_param(p, a, subst);
                }
            }
        }
        Type::Pick(p, pk) => {
            if let Type::Pick(a, ak) = aty {
                if pk == ak {
                    solve_param(p, a, subst);
                }
            }
        }
        Type::Merge(p1, p2) => {
            if let Type::Merge(a1, a2) = aty {
                solve_param(p1, a1, subst);
                solve_param(p2, a2, subst);
            }
        }
        // Nothing inside to descend into: a scalar, a SIMD value, a nominal
        // name, a logger handle, a const type argument, and the two types no
        // signature can spell (`Never`, `Err`).
        Type::Int
        | Type::IntN { .. }
        | Type::Float
        | Type::Float32
        | Type::F32x4
        | Type::I32x4
        | Type::F64x2
        | Type::Mask32x4
        | Type::Mask64x2
        | Type::Bool
        | Type::Str
        | Type::Unit
        | Type::Named(_)
        | Type::ConstInt(_)
        | Type::Logger
        | Type::Never
        | Type::Err => {}
    }
}
