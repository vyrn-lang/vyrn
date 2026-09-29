//! The JSON codec: what `toJson` and `fromJson` share at compile
//! time.
//!
//! 1. Codability: which types may cross the wire ([`encodable`],
//!    [`decodable`]), with the offender named, and the [`Wire`] form a type
//!    crosses as.
//! 2. The canonical escape ([`escape_into`]), the one table every encoder
//!    writes.
//!
//! Decode ignores unknown fields, reads an absent field or `null` as `None`,
//! parses integers exactly, and runs every `where` clause; failures
//! accumulate as `Issue`s instead of trapping.

use crate::ast::*;
use std::collections::HashMap;

/// Escapes a string into a JSON string body, without the quotes: `\" \\ \n
/// \t \r`, `\u00XX` for other control characters, everything else verbatim.
/// Every backend produces these exact bytes.
pub fn escape_into(s: &str, out: &mut String) {
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
}

/// What a type is on the JSON wire, once names, generic applications and record
/// transformers are resolved away.
///
/// The checker's gate ([`codable`]), the schema emitter
/// (`types::type_schema`) and the `TypeArg` builder read this one
/// answer. A record transformer is the record it computes
/// and an applied generic its substituted base, so both cross the wire.
///
/// A [`Type::Named`] is presented by each caller before it gets here, and they
/// differ: [`codable`] keeps a `seen` list to end a cycle, `type_schema` emits
/// a `$ref`, and the `TypeArg` builder gives a `where` type its own node over
/// its base.
#[derive(Debug, Clone, PartialEq)]
pub enum Wire {
    /// `Int64`: a JSON number in integer syntax.
    Int,
    /// A sized integer; its width is part of the wire contract.
    IntN {
        bits: u8,
        signed: bool,
    },
    Float,
    Float32,
    Bool,
    Str,
    /// `Option<T>`: the payload, or absent or `null`. Carries `T`.
    Option(Type),
    Array(Type),
    /// `Array<T, N>`: a JSON array, encode-only, because its length is unknown
    /// until the data arrives.
    FixedArray(Type, usize),
    /// `Map<String, V>`: a JSON object. Carries `V`.
    Map(Type),
    /// `Map<Int64, V>`: a JSON object keyed by the decimal texts of the keys,
    /// `{"7": 1}`. The decoder accepts canonical text only.
    MapI(Type),
    Record(Vec<Field>),
    /// A sum type in external tagging, `Result<T, E>` included.
    Enum(Vec<EnumVariant>),
}

/// Returns the wire form of `ty`, or `Err` with the resolved type that has
/// none. `decode` picks the direction: a fixed array and a `lazy` field are
/// encode-only.
pub fn wire(ty: &Type, types: &HashMap<String, TypeDecl>, decode: bool) -> Result<Wire, Type> {
    // `Validation<T>` has no wire form. It is refused by name, before
    // resolution, so the diagnostic says `Validation`.
    if is_validation(ty) {
        return Err(ty.clone());
    }
    // A `lazy T` field encodes as `T`: the read forces the thunk. It does not
    // decode: decoded data has no computation to defer.
    if let Type::Lazy(inner) = ty {
        return if decode {
            Err(ty.clone())
        } else {
            wire(inner, types, decode)
        };
    }
    // A nested `Option` has no wire form: a double `null` has two readings. An
    // `Option` of a payload enum does: a payload enum is never `null`.
    if let Some(inner) = crate::types::option_payload(ty) {
        if crate::types::option_payload(&crate::types::resolve(inner, types)).is_some() {
            return Err(ty.clone());
        }
    }
    let r = crate::types::resolve(ty, types);
    // `resolve` answers `Enum` for both built-in sums, which keep their own wire
    // forms: an `Option` is `null` or the value, and a `Result` lists `Ok` first,
    // because a wire tag is a name, not an index.
    if let Some(inner) = crate::types::option_payload(&r) {
        return Ok(Wire::Option(inner.clone()));
    }
    if let Some((t, e)) = crate::types::result_payloads(&r) {
        return Ok(Wire::Enum(vec![
            EnumVariant {
                name: "Ok".to_string(),
                payload: vec![t.clone()],
            },
            EnumVariant {
                name: "Err".to_string(),
                payload: vec![e.clone()],
            },
        ]));
    }
    match r {
        Type::Int => Ok(Wire::Int),
        Type::IntN { bits, signed } => Ok(Wire::IntN { bits, signed }),
        Type::Float => Ok(Wire::Float),
        Type::Float32 => Ok(Wire::Float32),
        Type::Bool => Ok(Wire::Bool),
        Type::Str => Ok(Wire::Str),
        Type::Array(inner) => Ok(Wire::Array(*inner)),
        Type::ArrayN(inner, n) => {
            if decode {
                Err(Type::ArrayN(inner, n))
            } else {
                Ok(Wire::FixedArray(*inner, n))
            }
        }
        // A map is a JSON object; the key type picks the spelling: `String` keys as
        // they are, `Int64` keys as decimal text. Checked programs reach no
        // other key type, but `wire` is also asked about imported and reflected types.
        Type::Map(key, val) => match crate::types::resolve(&key, types) {
            Type::Str => Ok(Wire::Map(*val)),
            Type::Int => Ok(Wire::MapI(*val)),
            _ => Err(Type::Map(key, val)),
        },
        Type::Record(fields) => Ok(Wire::Record(fields)),
        Type::Enum(vs) => Ok(Wire::Enum(vs)),
        // Everything else has no wire form: the vectors, `Ref`, `Task`, `Stream`,
        // `SmallArray`, `Template`, `Logger`, `Fn`, `Param`, `Unit`, `ConstInt`,
        // `Never`, `Err`, and an unresolved name.
        other => Err(other),
    }
}

/// Returns whether `ty` resolves through a head that can be cyclic, so a walk
/// over [`Wire`] must guard before re-entering it.
fn resolving_head(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Named(_)
            | Type::App(..)
            | Type::Omit(..)
            | Type::Pick(..)
            | Type::Merge(..)
            | Type::Partial(_)
    )
}

/// Checks that `toJson` may encode `ty`, or names the first non-codable type.
/// A fixed `Array<T, N>` encodes as an ordinary array.
pub fn encodable(ty: &Type, types: &HashMap<String, TypeDecl>) -> Result<(), String> {
    codable(ty, types, false, &mut Vec::new())
}

/// Checks that `ty` may be a `fromJson` target, or names the first
/// non-codable type. A fixed `Array<T, N>` cannot be decoded.
pub fn decodable(ty: &Type, types: &HashMap<String, TypeDecl>) -> Result<(), String> {
    codable(ty, types, true, &mut Vec::new())
}

/// The gate, as a walk over [`Wire`]: it asks only whether every leaf has a
/// wire form. The diagnostic stays here: a refusal names the user's spelling,
/// so a named type re-badges a structural refusal, and an enum payload names
/// its variant.
fn codable(
    ty: &Type,
    types: &HashMap<String, TypeDecl>,
    decode: bool,
    seen: &mut Vec<String>,
) -> Result<(), String> {
    let display = type_display(ty);
    if is_validation(ty) {
        return Err("Validation".to_string());
    }
    // A type that resolves through a name can be self-referential
    // (`type Node = { kids: Array<Node> }`), so the walk guards on the spelling
    // it re-enters. `Named` also re-badges the refusal.
    if let Type::Named(n) = ty {
        if seen.iter().any(|s| s == n) {
            return Ok(()); // break the cycle
        }
        let Some(d) = types.get(n) else {
            return Err(n.clone());
        };
        seen.push(n.clone());
        let r = codable_wire(ty, &display, types, decode, seen);
        seen.pop();
        // Keep a payload enum's variant offender; re-badge any other refusal with
        // the user's name.
        return r.map_err(|e| {
            if crate::types::declared_variants(&d.base)
                .is_some_and(|vs| vs.iter().any(|v| !v.payload.is_empty()))
            {
                e
            } else {
                n.clone()
            }
        });
    }
    if resolving_head(ty) {
        let key = ty.to_string();
        if seen.iter().any(|s| s == &key) {
            return Ok(());
        }
        seen.push(key);
        let r = codable_wire(ty, &display, types, decode, seen);
        seen.pop();
        return r.map_err(|_| display);
    }
    codable_wire(ty, &display, types, decode, seen)
}

/// `codable` after the cycle guard: asks [`wire`] what the type is and
/// recurses on what it carries. `display` is the user's spelling of `ty`,
/// lost after resolution.
fn codable_wire(
    ty: &Type,
    display: &str,
    types: &HashMap<String, TypeDecl>,
    decode: bool,
    seen: &mut Vec<String>,
) -> Result<(), String> {
    match wire(ty, types, decode) {
        Err(_) => Err(display.to_string()),
        Ok(
            Wire::Int | Wire::IntN { .. } | Wire::Float | Wire::Float32 | Wire::Bool | Wire::Str,
        ) => Ok(()),
        Ok(
            Wire::Option(inner)
            | Wire::Array(inner)
            | Wire::FixedArray(inner, _)
            | Wire::Map(inner)
            | Wire::MapI(inner),
        ) => codable(&inner, types, decode, seen),
        Ok(Wire::Record(fields)) => {
            for f in &fields {
                codable(&f.ty, types, decode, seen)?;
            }
            Ok(())
        }
        // A payload enum is codable when every payload is; the refusal
        // names the variant (`Task<Int64> (payload of variant `Boxed`)`).
        Ok(Wire::Enum(vs)) => enum_codable(&vs, types, decode, seen),
    }
}

/// Returns whether `ty` is spelled `Validation` or `Validation<..>`.
fn is_validation(ty: &Type) -> bool {
    match ty {
        Type::Named(n) => n == "Validation",
        Type::App(n, _) => n == "Validation",
        _ => false,
    }
}

/// Checks every variant's payloads; the error names the first offending
/// variant and payload type.
fn enum_codable(
    vs: &[EnumVariant],
    types: &HashMap<String, TypeDecl>,
    decode: bool,
    seen: &mut Vec<String>,
) -> Result<(), String> {
    for v in vs {
        for p in &v.payload {
            codable(p, types, decode, seen).map_err(|_| enum_payload_offender(p, &v.name))?;
        }
    }
    Ok(())
}

/// Returns the refusal for a non-codable enum payload:
/// `<type> (payload of variant `Name`)`.
fn enum_payload_offender(p: &Type, variant: &str) -> String {
    format!("{} (payload of variant `{}`)", type_display(p), variant)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One inhabitant of every variant of [`Type`], kept complete by
    /// [`Type::VARIANTS`] and [`Type::variant_name`].
    fn seed_per_variant() -> Vec<Type> {
        let b = |t: Type| Box::new(t);
        vec![
            Type::Int,
            Type::IntN {
                bits: 8,
                signed: false,
            },
            Type::Float,
            Type::Float32,
            Type::F32x4,
            Type::I32x4,
            Type::F64x2,
            Type::Mask32x4,
            Type::Mask64x2,
            Type::Bool,
            Type::Str,
            Type::Unit,
            Type::Named("R".into()),
            Type::Record(vec![Field {
                name: "a".into(),
                ty: Type::Int,
            }]),
            Type::Omit(b(Type::Named("R".into())), vec!["b".into()]),
            Type::Pick(b(Type::Named("R".into())), vec!["a".into()]),
            Type::Merge(b(Type::Named("R".into())), b(Type::Named("R".into()))),
            Type::Partial(b(Type::Named("R".into()))),
            Type::Enum(vec![EnumVariant {
                name: "A".into(),
                payload: vec![Type::Int],
            }]),
            Type::Param("T".into()),
            Type::App("Box".into(), vec![Type::Int]),
            Type::Array(b(Type::Int)),
            Type::ArrayN(b(Type::Int), 4),
            Type::SmallArray(b(Type::Int), 4),
            Type::ConstInt(8),
            Type::Map(b(Type::Str), b(Type::Int)),
            Type::Stream(b(Type::Int)),
            Type::Logger,
            Type::Fn(vec![Type::Int], b(Type::Unit)),
            Type::Lazy(b(Type::Int)),
            Type::Never,
            Type::Err,
        ]
    }

    fn seed_types() -> HashMap<String, TypeDecl> {
        let d = |name: &str, base: Type, params: Vec<String>| TypeDecl {
            name: name.to_string(),
            exported: false,
            module: None,
            doc: None,
            type_params: params,
            base,
            predicate: None,
            line: 0,
        };
        let mut m = HashMap::new();
        m.insert(
            "R".to_string(),
            d(
                "R",
                Type::Record(vec![
                    Field {
                        name: "a".into(),
                        ty: Type::Int,
                    },
                    Field {
                        name: "b".into(),
                        ty: Type::Str,
                    },
                ]),
                vec![],
            ),
        );
        m.insert(
            "Box".to_string(),
            d(
                "Box",
                Type::Record(vec![Field {
                    name: "value".into(),
                    ty: Type::Param("T".into()),
                }]),
                vec!["T".into()],
            ),
        );
        m
    }

    /// Every variant of [`Type`], and a few thousand trees built from them, gets
    /// a wire verdict in both directions without a panic, and the gate never
    /// admits what [`wire`] refuses.
    ///
    /// The cases derive from the type enum: [`Type::variant_name`] is exhaustive,
    /// so a new variant stops this file compiling, and [`Type::VARIANTS`] fails a
    /// variant with no seed.
    #[test]
    fn every_type_variant_has_one_wire_verdict() {
        let types = seed_types();
        let seeds = seed_per_variant();
        let seeded: std::collections::BTreeSet<&str> =
            seeds.iter().map(|t| t.variant_name()).collect();
        for v in Type::VARIANTS {
            assert!(seeded.contains(v), "no wire seed for Type::{v}");
        }
        assert!(
            seeded.iter().all(|s| Type::VARIANTS.contains(s)),
            "a seed names a variant Type::VARIANTS does not list"
        );

        // Every container over every seed, and every ordered pair through the
        // two-argument constructors.
        let mut all = seeds.clone();
        for t in &seeds {
            let b = || Box::new(t.clone());
            all.extend([
                Type::option(t.clone()),
                Type::Array(b()),
                Type::ArrayN(b(), 3),
                Type::Lazy(b()),
                Type::Map(Box::new(Type::Str), b()),
                Type::Record(vec![Field {
                    name: "f".into(),
                    ty: t.clone(),
                }]),
                Type::Enum(vec![EnumVariant {
                    name: "V".into(),
                    payload: vec![t.clone()],
                }]),
            ]);
        }
        let pairs: Vec<Type> = all[..12].to_vec();
        for a in &pairs {
            for c in &pairs {
                all.push(Type::result(a.clone(), c.clone()));
                all.push(Type::option(Type::result(a.clone(), c.clone())));
            }
        }
        assert!(all.len() > 500, "coverage shrank to {}", all.len());

        for ty in &all {
            for decode in [false, true] {
                let _ = wire(ty, &types, decode);
                // The gate refuses exactly when the leaf it walks to has no wire form.
                let gate = codable(ty, &types, decode, &mut Vec::new());
                if gate.is_ok() {
                    assert!(
                        wire(ty, &types, decode).is_ok(),
                        "the gate admits {ty} ({}) but it has no wire form",
                        if decode { "decode" } else { "encode" }
                    );
                }
            }
        }
    }

    /// A record transformer and an applied generic cross the wire as records.
    #[test]
    fn a_resolved_shape_crosses_the_wire() {
        let types = seed_types();
        for ty in [
            Type::App("Box".into(), vec![Type::Int]),
            Type::Omit(Box::new(Type::Named("R".into())), vec!["b".into()]),
            Type::Pick(Box::new(Type::Named("R".into())), vec!["a".into()]),
            Type::Merge(
                Box::new(Type::Named("R".into())),
                Box::new(Type::Named("R".into())),
            ),
            Type::Partial(Box::new(Type::Named("R".into()))),
        ] {
            assert!(encodable(&ty, &types).is_ok(), "{ty} does not encode");
            assert!(decodable(&ty, &types).is_ok(), "{ty} does not decode");
            assert!(
                matches!(wire(&ty, &types, false), Ok(Wire::Record(_))),
                "{ty} is not a record on the wire"
            );
        }
    }

    /// A nested `Option` is refused through an alias too:
    /// `type MaybeInt = Option<Int64>` must not make `Some(None)` and `None` both
    /// write `null`.
    #[test]
    fn a_nested_option_is_refused_through_an_alias() {
        let mut types = seed_types();
        types.insert(
            "MaybeInt".to_string(),
            TypeDecl {
                name: "MaybeInt".to_string(),
                exported: false,
                module: None,
                doc: None,
                type_params: Vec::new(),
                base: Type::option(Type::Int),
                predicate: None,
                line: 0,
            },
        );
        let aliased = Type::option(Type::Named("MaybeInt".into()));
        let bare = Type::option(Type::option(Type::Int));
        for ty in [&aliased, &bare] {
            assert!(encodable(ty, &types).is_err(), "{ty} encodes");
            assert!(decodable(ty, &types).is_err(), "{ty} decodes");
            assert!(wire(ty, &types, false).is_err(), "{ty} has a wire form");
        }
        // A single `Option` through the alias is fine.
        assert!(encodable(&Type::Named("MaybeInt".into()), &types).is_ok());
    }

    /// A self-referential type terminates through every resolving head, not only
    /// a name: `Partial<L>` of `type L = { next: L }` must not walk forever.
    #[test]
    fn a_cyclic_type_terminates_through_every_resolving_head() {
        let mut types = seed_types();
        types.insert(
            "L".to_string(),
            TypeDecl {
                name: "L".to_string(),
                exported: false,
                module: None,
                doc: None,
                type_params: Vec::new(),
                base: Type::Record(vec![Field {
                    name: "next".into(),
                    ty: Type::Array(Box::new(Type::Named("L".into()))),
                }]),
                predicate: None,
                line: 0,
            },
        );
        for ty in [
            Type::Named("L".into()),
            Type::Partial(Box::new(Type::Named("L".into()))),
            Type::Pick(Box::new(Type::Named("L".into())), vec!["next".into()]),
        ] {
            assert!(encodable(&ty, &types).is_ok(), "{ty} does not encode");
        }
    }

    /// A `Map<String, V>` is codable exactly when `V` is.
    #[test]
    fn map_codability_follows_the_value_type() {
        let types = HashMap::new();
        let ok = Type::Map(Box::new(Type::Str), Box::new(Type::Int));
        assert!(encodable(&ok, &types).is_ok());
        assert!(decodable(&ok, &types).is_ok());

        let nested = Type::Map(
            Box::new(Type::Str),
            Box::new(Type::Array(Box::new(Type::Int))),
        );
        assert!(encodable(&nested, &types).is_ok());
        assert!(decodable(&nested, &types).is_ok());

        // A non-codable value type (a stored `fn`) makes the map non-codable, and
        // names the offender.
        let bad = Type::Map(
            Box::new(Type::Str),
            Box::new(Type::Fn(vec![Type::Int], Box::new(Type::Unit))),
        );
        assert_eq!(encodable(&bad, &types).unwrap_err(), "fn(Int64)");
    }
}

/// Returns the user-facing spelling of a type for a codability refusal.
fn type_display(ty: &Type) -> String {
    match ty {
        Type::Named(n) => n.clone(),
        // The two built-in sums, read through their payloads, however the sum was
        // built.
        _ if crate::types::result_payloads(ty).is_some() => "Result".to_string(),
        Type::Logger => "Logger".to_string(),
        Type::ArrayN(inner, n) => format!("Array<{}, {}>", type_display(inner), n),
        _ if crate::types::option_payload(ty).is_some() => format!(
            "Option<{}>",
            type_display(crate::types::option_payload(ty).expect("an Option payload"))
        ),
        Type::Unit => "Unit".to_string(),
        other => format!("{other}"),
    }
}
