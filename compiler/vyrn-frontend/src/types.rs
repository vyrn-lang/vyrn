//! Type facts shared by the checker, the ownership passes and the backends:
//! structural resolution ([`resolve`], which erases `Omit`/`Pick`/`Merge`),
//! assignability, the protocol names the compiler knows and their impl
//! lookups, `where`-predicate reflection (`schemaOf`, `jsonSchema`), and the
//! monomorphization bounds.

use std::collections::HashMap;

use crate::ast::*;
use crate::codec::Wire;
use crate::prim::Cmp;

/// Returns the structural identity of a value's `Debug` form: 64 bits of
/// SHA-256, as hex. Every synthesizer that names a function after a type
/// appends it, because the readable spelling (`mangle_ty`) is not injective
/// and the name is also the worklist's dedup key. `Debug` over [`Type`] is a
/// total, injective rendering; it is not a stable format, so no artifact
/// outside the emitted module may carry a key, and `struct_key_is_pinned` pins
/// a table. A 64-bit collision is detected: the `TypeArg` builder keeps the
/// type beside the key.
pub fn struct_key(x: &impl std::fmt::Debug) -> String {
    crate::hash::sha256_hex(format!("{x:?}").as_bytes())[..16].to_string()
}

/// Bounds resolution of a cyclic alias (`type A = Omit<A, x>`). Deeper than
/// this yields `Unit`, a type error downstream rather than a stack overflow.
const MAX_DEPTH: usize = 64;

/// The target type of a numeric conversion `Name(x)` such as `Int32(x)`, or
/// `None` if `name` is not a numeric type name.
pub fn numeric_conv_target(name: &str) -> Option<Type> {
    match name {
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

/// `I32x4`'s lane type. Signed: the lane type, not the operation, picks the
/// signed form of each wasm `min`/`max`/comparison pair.
pub const INT32: Type = Type::IntN {
    bits: 32,
    signed: true,
};

/// The lane `v.lane(k)` reads, or `None` when `k` is not a compile-time constant
/// in `0..lanes`. The checker refuses `None`, so no backend emits a bounds
/// check.
pub fn const_lane(idx: &Expr, lanes: i64) -> Option<u8> {
    match crate::consteval::eval(idx, &HashMap::new()) {
        Some(crate::consteval::ConstVal::Int(k)) if k >= 0 && k < lanes => Some(k as u8),
        _ => None,
    }
}

/// The key a type has as a protocol-impl target, or `None` for a structural
/// type. A generic type keys on its constructor alone, so one impl serves every
/// instance and two impls for one constructor collide.
pub fn type_key(ty: &Type) -> Option<String> {
    match ty {
        Type::Int => Some("Int64".to_string()),
        Type::Bool => Some("Bool".to_string()),
        Type::Str => Some("String".to_string()),
        Type::Named(n) => Some(n.clone()),
        _ if option_payload(ty).is_some() => Some("Option".to_string()),
        _ if result_payloads(ty).is_some() => Some("Result".to_string()),
        Type::App(n, _) => Some(n.clone()),
        _ => None,
    }
}

/// The flattened name of an `impl` method, e.g. `Show$Int64$show`. `$` is no
/// identifier character, so no source spells one. A type key may hold `$`
/// (`Copy$json$Json$copy`); a protocol or method name never does, so the first
/// and the last `$` delimit the key.
pub fn impl_method_name(protocol: &str, type_key: &str, method: &str) -> String {
    debug_assert!(
        !protocol.contains('$') && !method.contains('$'),
        "`impl {protocol}` method `{method}` holds the flattening separator"
    );
    format!("{protocol}${type_key}${method}")
}

/// The type key of the `protocol` method `method` that `name` flattens, or
/// `None` for any other name.
pub fn impl_method_key<'a>(name: &'a str, protocol: &str, method: &str) -> Option<&'a str> {
    let rest = name.strip_prefix(protocol)?.strip_prefix('$')?;
    rest.strip_suffix(method)?.strip_suffix('$')
}

/// The protocol member an [`impl_method_name`] names, or `None` for a name
/// with fewer than two `$`. A derived name (`derive$g$f`) reads as `f`.
pub fn impl_method_member(name: &str) -> Option<&str> {
    let (_, rest) = name.split_once('$')?;
    rest.rsplit_once('$').map(|(_, m)| m)
}

/// The protocol `?` resolves through for an operand that is neither `Option`
/// nor `Result`. The compiler knows these protocol names and their
/// method names only, so a bare file with no std can declare one itself.
/// `Option` and `Result` do not route through it: `?` on a `Result` must check
/// the error type, which `Fallible` cannot state, and they lower to an inline
/// tag test.
pub const FALLIBLE: &str = "Fallible";

/// The protocol through which a type says how it is released.
pub const OWNED: &str = "Owned";

pub const OWNED_RELEASE: &str = "release";

/// The protocol through which a type says a value must be disposed of by name,
/// the obligation `Stream` has. It declares no methods: the release
/// is [`OWNED`]'s. It differs from `consume`, which is a calling convention,
/// not an obligation on a type.
pub const MUST_USE: &str = "MustUse";

/// The protocol through which a type overrides the structural `x.copy()`.
pub const COPY: &str = "Copy";

pub const COPY_COPY: &str = "copy";

/// The protocol through which a type says how it renders as text. A scalar
/// always renders by the language's lowering, so `impl Show for Int64` changes
/// nothing (and `examples/protocol.vyrn`'s `self.toString()` body cannot
/// recurse).
pub const SHOW: &str = "Show";

pub const SHOW_SHOW: &str = "show";

/// Whether the language renders `t` itself; such a type never reaches
/// [`show_dispatch`].
pub fn renders(t: &Type) -> bool {
    matches!(
        t,
        Type::Int | Type::IntN { .. } | Type::Float | Type::Float32 | Type::Bool | Type::Str
    )
}

/// The refusal of `shown` at `t`, a type that does not render.
pub fn needs_show(shown: &str, t: &Type) -> crate::rules::Rule {
    use crate::rules::{rule, DeclName};
    let (shown, found) = (DeclName(shown), t);
    match show_key(t) {
        Some(key) => rule!(NeedsShowImpl, shown, found, key = DeclName(&key)),
        None => rule!(NeedsShow, shown, found),
    }
}

/// The type key a refusal names in its `impl Show` hint. A module-prefixed
/// type has no spelling at the call site, so it gets no hint.
pub fn show_key(t: &Type) -> Option<String> {
    type_key(t).filter(|k| !k.contains('$'))
}

/// The `impl Show` method a value renders through, or `None` where the
/// language renders it or nothing is declared. The impl is keyed by `written`;
/// [`renders`] asks `base`, the resolved type, so `type Email = String`
/// renders as a String.
pub fn show_dispatch(impls: &[ImplBlock], written: &Type, base: &Type) -> Option<String> {
    if renders(base) {
        return None;
    }
    show_impl_by_key(impls, &type_key(written)?)
}

/// [`show_dispatch`]'s impl, by type key.
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

/// The protocol through which a user container is iterated by `for x in xs`:
/// `fn size(read self) -> Int64` and the projection
/// `fn nth(read self, i: Int64) -> read Item`.
pub const ITERATE: &str = "Iterate";

pub const ITERATE_SIZE: &str = "size";

/// A projection, not a method: it is found in the impl's `places`.
pub const ITERATE_NTH: &str = "nth";

/// The protocols a program implements without declaring them, each with its
/// methods' receiver and parameter capabilities. An impl of one takes exactly
/// these, as an impl of a declared protocol takes its declaration's.
pub const KNOWN_PROTOCOLS: &[(&str, &[(&str, Capability, &[Capability])])] = &[
    (OWNED, &[(OWNED_RELEASE, Capability::Consume, &[])]),
    (MUST_USE, &[]),
    (COPY, &[(COPY_COPY, Capability::Read, &[])]),
    (SHOW, &[(SHOW_SHOW, Capability::Read, &[])]),
    (
        "Index",
        &[
            ("at", Capability::Read, &[Capability::Read]),
            ("atSet", Capability::Modify, &[Capability::Read]),
        ],
    ),
    (
        ITERATE,
        &[
            (ITERATE_SIZE, Capability::Read, &[]),
            (ITERATE_NTH, Capability::Read, &[Capability::Read]),
        ],
    ),
    (HASHABLE, &[("hash", Capability::Read, &[])]),
    (
        FALLIBLE,
        &[
            ("isSuccess", Capability::Read, &[]),
            ("success", Capability::Read, &[]),
        ],
    ),
];

/// The protocol that admits a user type as a `Map` key.
pub const HASHABLE: &str = "Hashable";

/// Whether `ty` declares `impl Hashable`. The runtime never calls the impl:
/// the map hashes the canonical key bytes itself.
pub fn hashable_impl(impls: &[ImplBlock], ty: &Type) -> bool {
    type_key(ty).is_some_and(|k| {
        impls.iter().any(|i| {
            i.protocol == HASHABLE
                && type_key(&i.ty).as_deref() == Some(&k)
                && i.methods.iter().any(|m| m.name == "hash")
        })
    })
}

/// The `impl Copy` method `ty` dispatches to, or `None` where the copy stays
/// structural.
pub fn copy_impl(impls: &[ImplBlock], ty: &Type) -> Option<String> {
    copy_impl_by_key(impls, &type_key(ty)?)
}

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

/// Returns the `impl Iterate` of `ty`, its flattened `size` name and its
/// `place nth`, or `None` unless the impl has both.
pub fn iterate_impl<'a>(
    impls: &'a [ImplBlock],
    ty: &Type,
) -> Option<(&'a ImplBlock, String, &'a Function)> {
    let key = type_key(ty)?;
    let imp = impls
        .iter()
        .find(|i| i.protocol == ITERATE && type_key(&i.ty).as_deref() == Some(&key))?;
    let nth = imp.places.iter().find(|f| f.name == ITERATE_NTH)?;
    imp.methods.iter().find(|m| m.name == ITERATE_SIZE)?;
    Some((imp, impl_method_name(ITERATE, &key, ITERATE_SIZE), nth))
}

/// The inclusive `(min, max)` a `where` predicate implies, from `value OP N`
/// comparisons in either order under `&&`. `N` may be negated or a byte
/// literal; any other clause contributes no bound.
pub fn predicate_bounds(pred: &Expr) -> (Option<i64>, Option<i64>) {
    if let Expr::Binary { op, lhs, rhs, .. } = pred {
        if *op == BinOp::And {
            let (l0, l1) = predicate_bounds(lhs);
            let (r0, r1) = predicate_bounds(rhs);
            return (l0.or(r0), l1.or(r1));
        }
        return match value_cmp(*op, lhs, rhs, is_value, int_lit) {
            Some((c, n)) => c.bounds(n),
            None => (None, None),
        };
    }
    (None, None)
}

/// The comparison `lhs OP rhs` read as `v OP' n`, where `is_v` picks the
/// side `v` and `lit` reads the other.
fn value_cmp<T>(
    op: BinOp,
    lhs: &Expr,
    rhs: &Expr,
    is_v: fn(&Expr) -> bool,
    lit: fn(&Expr) -> Option<T>,
) -> Option<(Cmp, T)> {
    let c = op.compare()?;
    match (lhs, rhs) {
        (l, r) if is_v(l) => Some((c, lit(r)?)),
        (l, r) if is_v(r) => Some((c.converse(), lit(l)?)),
        _ => None,
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
                        && matches!(&**rhs, Expr::Int(0, _))
                    {
                        if let Expr::Int(kv, _) = &**k {
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
/// `value.byteLength OP N` comparisons.
pub fn predicate_length_bounds(pred: &Expr) -> (Option<i64>, Option<i64>) {
    if let Expr::Binary { op, lhs, rhs, .. } = pred {
        if *op == BinOp::And {
            let (l0, l1) = predicate_length_bounds(lhs);
            let (r0, r1) = predicate_length_bounds(rhs);
            return (l0.or(r0), l1.or(r1));
        }
        if let Some((c, n)) = value_cmp(*op, lhs, rhs, is_length_of_value, int_lit) {
            return c.bounds(n);
        }
    }
    (None, None)
}

/// The pairs of names a predicate conjunction states equal lengths of:
/// `a.length == b.length`, either side `byteLength`.
pub fn predicate_equal_lengths(pred: &Expr) -> Vec<(String, String)> {
    let Expr::Binary { op, lhs, rhs, .. } = pred else {
        return Vec::new();
    };
    let length_of = |e: &Expr| match e {
        Expr::Field { expr, field, .. } if field == "length" || field == "byteLength" => {
            match &**expr {
                Expr::Var { name, .. } => Some(name.clone()),
                _ => None,
            }
        }
        _ => None,
    };
    match op {
        BinOp::And => {
            let mut out = predicate_equal_lengths(lhs);
            out.extend(predicate_equal_lengths(rhs));
            out
        }
        BinOp::Eq => length_of(lhs).zip(length_of(rhs)).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// The first `value =~ "..."` pattern in a predicate conjunction, unanchored.
pub fn predicate_pattern(pred: &Expr) -> Option<String> {
    if let Expr::Binary { op, lhs, rhs, .. } = pred {
        match op {
            BinOp::And => return predicate_pattern(lhs).or_else(|| predicate_pattern(rhs)),
            BinOp::Match => {
                if matches!(&**lhs, Expr::Var { name, .. } if name == "value") {
                    if let Expr::Str(pat, _) = &**rhs {
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
        Type::Enum(_) if !is_sum_alias(ty) => "enum",
        _ => "?",
    }
}

/// Builds the `Schema { .. }` literal `schemaOf<T>()` reflects: the type's
/// name, base spelling, `///` doc, and what its `where` predicate implies.
pub fn schema_struct_lit(decl: &TypeDecl) -> Expr {
    let pred = decl.predicate.as_ref();
    let (min, max) = pred.map_or((None, None), |p| predicate_bounds(p));
    let (min_len, max_len) = pred.map_or((None, None), |p| predicate_length_bounds(p));
    let multiple_of = pred.and_then(predicate_multiple_of);
    let pattern = pred.and_then(predicate_pattern);
    let opt = |n: Option<i64>| match n {
        Some(v) => Expr::Call {
            id: Id::NEW,
            dot: false,
            type_args: Vec::new(),
            name: "Some".to_string(),
            args: vec![Expr::Int(v, Id::NEW)],
            line: 0,
        },
        None => Expr::Var {
            id: Id::NEW,
            name: "None".to_string(),
            line: 0,
        },
    };
    let opt_str = |s: Option<String>| match s {
        Some(v) => Expr::Call {
            id: Id::NEW,
            dot: false,
            type_args: Vec::new(),
            name: "Some".to_string(),
            args: vec![Expr::Str(v, Id::NEW)],
            line: 0,
        },
        None => Expr::Var {
            id: Id::NEW,
            name: "None".to_string(),
            line: 0,
        },
    };
    Expr::StructLit {
        id: Id::NEW,
        name: "Schema".to_string(),
        fields: vec![
            ("name".to_string(), Expr::Str(decl.name.clone(), Id::NEW)),
            (
                "base".to_string(),
                Expr::Str(base_spelling(&decl.base).to_string(), Id::NEW),
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

/// Renders the JSON Schema (draft 2020-12) document `jsonSchema<T>()` folds
/// to. A record is an `object` whose `required` list holds its non-`Option`
/// fields; a `where` predicate contributes its bounds.
pub fn json_schema_string(decl: &TypeDecl, types: &dyn Decls) -> String {
    let dialect = "\"$schema\":\"https://json-schema.org/draft/2020-12/schema\"";
    let mut cx = SchemaCx {
        types,
        root: &decl.name,
        defs: Vec::new(),
    };
    // Inside the expansion `Named(root)` is a back-edge: `{"$ref":"#"}`.
    let inner = named_schema(decl, &mut cx);
    // `$defs` goes last, as the JSON Schema importer writes it back.
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
        // The dialect goes first.
        format!("{{{dialect},{}", &body[1..])
    }
}

struct SchemaCx<'a> {
    types: &'a dyn Decls,
    /// A back-edge to the root renders `{"$ref":"#"}`.
    root: &'a str,
    /// Every non-root named type in first-encounter order, with its schema.
    /// `None` means it is being rendered, so a cycle takes the `$ref` and
    /// terminates.
    defs: Vec<(String, Option<String>)>,
}

/// The JSON Schema object for a type. A named type renders as a `$ref` into
/// [`SchemaCx::defs`], except a synthetic refinement helper (`User.age`),
/// which stays inline. [`crate::codec::wire`] decides the shape; this only
/// spells it.
fn type_schema(ty: &Type, cx: &mut SchemaCx) -> String {
    match ty {
        Type::Named(n) => {
            if n == cx.root {
                return "{\"$ref\":\"#\"}".to_string();
            }
            let types = cx.types;
            if n.contains('.') {
                return match types.decl(n) {
                    Some(d) => named_schema(d, cx),
                    None => "{}".to_string(),
                };
            }
            match types.decl(n) {
                Some(d) => {
                    if !cx.defs.iter().any(|(dn, _)| dn == n) {
                        let i = cx.defs.len();
                        cx.defs.push((n.clone(), None));
                        let body = named_schema(d, cx);
                        cx.defs[i].1 = Some(body);
                    }
                    format!("{{\"$ref\":\"#/$defs/{}\"}}", json_escape(n))
                }
                None => "{}".to_string(),
            }
        }
        // A schema describes what is written: the encode direction.
        _ => match crate::codec::wire(ty, cx.types, false) {
            // A sized int's width bounds are part of the contract; Int64's are
            // not.
            Ok(Wire::Int) => "{\"type\":\"integer\"}".to_string(),
            Ok(Wire::IntN { bits, signed }) => intn_schema(bits, signed, &[]),
            Ok(Wire::Float | Wire::Float32) => "{\"type\":\"number\"}".to_string(),
            Ok(Wire::Bool) => "{\"type\":\"boolean\"}".to_string(),
            Ok(Wire::Str) => "{\"type\":\"string\"}".to_string(),
            // Optionality is omission from the object's `required` list.
            Ok(Wire::Option(inner)) => type_schema(&inner, cx),
            Ok(Wire::Array(inner) | Wire::FixedArray(inner, _)) => {
                format!(
                    "{{\"type\":\"array\",\"items\":{}}}",
                    type_schema(&inner, cx)
                )
            }
            Ok(Wire::Map(val)) => {
                format!(
                    "{{\"type\":\"object\",\"additionalProperties\":{}}}",
                    type_schema(&val, cx)
                )
            }
            // Keys are canonical decimal texts.
            Ok(Wire::MapI(val)) => {
                format!(
                    "{{\"type\":\"object\",\"propertyNames\":{{\"pattern\":\"^-?(0|[1-9][0-9]*)$\"}},\"additionalProperties\":{}}}",
                    type_schema(&val, cx)
                )
            }
            Ok(Wire::Record(fields)) => record_schema(&fields, cx),
            // A payload-less sum is an `enum` of its names; any other (and
            // `Result`) is the externally tagged `oneOf`.
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
            // No wire form: `{}`, "anything".
            Err(_) => "{}".to_string(),
        },
    }
}

/// The `oneOf` schema for a payload enum, in declaration order: a nullary
/// variant is a `const`, a single payload a one-property object, a tuple
/// payload `prefixItems` with `"items":false`.
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

/// The schema for a sized integer: its width bounds, where `extra` (the
/// predicate's constraints) does not already bound that side. `UInt64` gets no
/// `maximum`: 2^64 - 1 is no `Int64` literal, so it cannot be re-imported.
fn intn_schema(bits: u8, signed: bool, extra: &[(String, String)]) -> String {
    const BOUND_KEYS: [&str; 4] = ["minimum", "exclusiveMinimum", "maximum", "exclusiveMaximum"];
    let mut parts = vec!["\"type\":\"integer\"".to_string()];
    let has = |k: &str| extra.iter().any(|(ek, _)| ek == k);
    // The importer's canonical key order, so emit, import, re-emit is byte
    // stable. Shifting the i64 extremes works at every width; `1 << 64` would
    // overflow.
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
/// constraints; anything else defers to its base.
fn named_schema(decl: &TypeDecl, cx: &mut SchemaCx) -> String {
    let pred = decl.predicate.as_ref();
    match &decl.base {
        Type::Int => scalar_with_constraints("integer", pred),
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
        // JSON Schema cannot relate properties, so a record's `where` becomes
        // a `$comment`.
        Type::Record(fields) if pred.is_some() => {
            let obj = record_schema(fields, cx);
            let comment = unmapped_comment(pred.unwrap());
            format!("{}{}}}", &obj[..obj.len() - 1], format!(",{comment}"))
        }
        other => type_schema(other, cx),
    }
}

/// The schema for a `String` refinement: `value.byteLength` bounds become
/// `minLength`/`maxLength` and `value =~` patterns `pattern` (several under
/// `allOf`); anything else, a `$comment`. `byteLength` counts bytes and JSON
/// Schema counts code points, so the two agree on ASCII only; the runtime check
/// is the source of truth.
fn string_with_constraints(pred: Option<&Expr>) -> String {
    let mut parts = vec!["\"type\":\"string\"".to_string()];
    if let Some(p) = pred {
        let mut cs = Vec::new();
        let complete = collect_string_constraints(p, &mut cs);
        // An object allows one `pattern`.
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

/// Collects a `String` predicate's length bounds and patterns, returning
/// whether it was captured in full.
fn collect_string_constraints(pred: &Expr, out: &mut Vec<(String, String)>) -> bool {
    let Expr::Binary { op, lhs, rhs, .. } = pred else {
        return false;
    };
    if *op == BinOp::And {
        let a = collect_string_constraints(lhs, out);
        let b = collect_string_constraints(rhs, out);
        return a && b;
    }
    // `=~` is a full match, so the pattern is anchored. Vyrn's regex subset
    // means the same in ECMA-262.
    if *op == BinOp::Match {
        if is_value(lhs) {
            if let Expr::Str(pat, _) = &**rhs {
                out.push((
                    "pattern".to_string(),
                    format!("\"{}\"", json_escape(&format!("^{pat}$"))),
                ));
                return true;
            }
        }
        return false;
    }
    match value_cmp(*op, lhs, rhs, is_length_of_value, int_lit).map(|(c, n)| c.bounds(n)) {
        Some((Some(n), _)) => push_true(out, "minLength", n.to_string()),
        Some((_, Some(n))) => push_true(out, "maxLength", n.to_string()),
        _ => false,
    }
}

fn is_length_of_value(e: &Expr) -> bool {
    matches!(e, Expr::Field { expr, field, .. } if field == "byteLength" && is_value(expr))
}

/// An integer or byte literal, possibly negated, as an `i64`.
fn int_lit(e: &Expr) -> Option<i64> {
    match e {
        Expr::Int(n, _) => Some(*n),
        Expr::Byte(b, _) => Some(*b as i64),
        Expr::Unary {
            op: UnOp::Neg,
            expr,
            ..
        } => match &**expr {
            Expr::Int(n, _) => Some(-n),
            Expr::Byte(b, _) => Some(-(*b as i64)),
            _ => None,
        },
        _ => None,
    }
}

/// The schema for a scalar refinement. A predicate the keywords cannot capture
/// in full (a disjunction) adds a `$comment` with the exact source predicate.
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

fn unmapped_comment(pred: &Expr) -> String {
    let text = format!("constrained by: {}", crate::checker::pred_summary(pred));
    format!("\"$comment\":\"{}\"", json_escape(&text))
}

/// Escapes a JSON string value with [`crate::codec::escape_into`], the table
/// the encoders and the strict reader share.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    crate::codec::escape_into(s, &mut out);
    out
}

/// A record is an `object`; its non-`Option` fields are `required`.
fn record_schema(fields: &[Field], cx: &mut SchemaCx) -> String {
    // `toJson` forces a `lazy T` field, so it is a `T` on the wire.
    let props: Vec<String> = fields
        .iter()
        .map(|f| format!("\"{}\":{}", f.name, type_schema(&forced(&f.ty), cx)))
        .collect();
    let required: Vec<String> = fields
        .iter()
        .filter(|f| option_payload(&forced(&f.ty)).is_none())
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

/// Collects a `where` predicate's numeric constraints (bounds, `multipleOf`,
/// `not: {const}`) under `&&`, returning whether it was captured in full.
fn collect_constraints(pred: &Expr, out: &mut Vec<(String, String)>) -> bool {
    let Expr::Binary { op, lhs, rhs, .. } = pred else {
        return false;
    };
    match op {
        BinOp::And => {
            let a = collect_constraints(lhs, out);
            let b = collect_constraints(rhs, out);
            a && b
        }
        // Only `value % K == 0` maps, to `multipleOf`.
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
        _ => match value_cmp(*op, lhs, rhs, is_value, num_lit) {
            Some((Cmp::Order { strict, flipped }, n)) => {
                let key = match (flipped, strict) {
                    (false, false) => "minimum",
                    (false, true) => "exclusiveMinimum",
                    (true, false) => "maximum",
                    (true, true) => "exclusiveMaximum",
                };
                push_true(out, key, n)
            }
            _ => false,
        },
    }
}

fn push_true(out: &mut Vec<(String, String)>, key: &str, val: String) -> bool {
    out.push((key.to_string(), val));
    true
}

fn is_value(e: &Expr) -> bool {
    matches!(e, Expr::Var { name, .. } if name == "value")
}

fn is_zero(e: &Expr) -> bool {
    matches!(e, Expr::Int(0, _)) || matches!(e, Expr::Float(f, _) if *f == 0.0)
}

/// A numeric literal, possibly negated, as a JSON number.
fn num_lit(e: &Expr) -> Option<String> {
    match e {
        Expr::Int(n, _) => Some(n.to_string()),
        // A literal is finite, so `{}` is valid JSON.
        Expr::Float(f, _) => Some(format!("{f}")),
        Expr::Unary {
            op: UnOp::Neg,
            expr,
            ..
        } => match &**expr {
            Expr::Int(n, _) => Some((-n).to_string()),
            Expr::Float(f, _) => Some(format!("{}", -f)),
            _ => None,
        },
        _ => None,
    }
}

/// Visits `ty` and every type inside it.
pub fn walk_type(ty: &Type, f: &mut impl FnMut(&Type)) {
    f(ty);
    match ty {
        Type::Array(a)
        | Type::Stream(a)
        | Type::Partial(a)
        | Type::Lazy(a)
        | Type::ArrayN(a, _)
        | Type::SmallArray(a, _)
        | Type::Omit(a, _)
        | Type::Pick(a, _) => walk_type(a, f),
        Type::Merge(a, b) | Type::Map(a, b) => {
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

/// How deeply `ty` nests: `Int64` is 1, `Array<Int64>` is 2.
pub fn type_depth(ty: &Type) -> usize {
    fn deepest(ts: impl Iterator<Item = usize>) -> usize {
        ts.max().unwrap_or(0)
    }
    1 + match ty {
        Type::Array(a)
        | Type::Stream(a)
        | Type::Partial(a)
        | Type::Lazy(a)
        | Type::ArrayN(a, _)
        | Type::SmallArray(a, _)
        | Type::Omit(a, _)
        | Type::Pick(a, _) => type_depth(a),
        Type::Merge(a, b) | Type::Map(a, b) => type_depth(a).max(type_depth(b)),
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

/// The deepest a type may nest in an instantiation. Polymorphic recursion
/// (`f<T>` calling `f<P<T>>`) has no fixed point, and finitely many
/// constructors under a depth bound admit finitely many types, so this bounds
/// the monomorphization worklists. The corpus peaks in single digits.
pub const MONO_DEPTH_LIMIT: usize = 64;

/// The most parts an instantiated type may have once expanded
/// ([`expanded_size`]). Depth alone is not enough: `type P<T> = { a: T, b: T }`
/// nested d deep has 2^d leaves. About a thousand times the corpus's largest.
pub const MONO_SIZE_LIMIT: usize = 65_536;

/// How many parts `ty` has once every named type is expanded, or `None` past
/// `budget`; the walk stops there. A name that recurs at the same
/// [`type_depth`] (`Node` in `Node`) counts as a leaf; `P<X>` in `P<P<X>>` does
/// not.
pub fn expanded_size(ty: &Type, types: &dyn Decls, budget: usize) -> Option<usize> {
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
    types: &dyn Decls,
    budget: usize,
    n: &mut usize,
    depth: usize,
    seen: &mut Vec<(String, usize)>,
) -> bool {
    *n += 1;
    // The budget terminates the walk; the depth bound protects the stack.
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
        Type::Array(a)
        | Type::Stream(a)
        | Type::Partial(a)
        | Type::Lazy(a)
        | Type::ArrayN(a, _)
        | Type::SmallArray(a, _)
        | Type::Omit(a, _)
        | Type::Pick(a, _) => size_go(a, types, budget, n, d, seen),
        Type::Merge(a, b) | Type::Map(a, b) => {
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

/// Replaces type parameters in `ty` with their bindings in `subst`.
pub fn substitute(ty: &Type, subst: &HashMap<String, Type>) -> Type {
    match ty {
        Type::Param(t) => subst.get(t).cloned().unwrap_or_else(|| ty.clone()),
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
        Type::Stream(inner) => Type::Stream(Box::new(substitute(inner, subst))),
        Type::Fn(params, ret) => Type::Fn(
            params.iter().map(|p| substitute(p, subst)).collect(),
            Box::new(substitute(ret, subst)),
        ),
        Type::Lazy(inner) => Type::Lazy(Box::new(substitute(inner, subst))),
        other => other.clone(),
    }
}

/// The type declarations a helper reads by name. A map reads and records
/// nothing; the checker records each lookup as a read of the body it checks
/// ([`crate::ast::Key`]).
pub trait Decls {
    fn decl(&self, name: &str) -> Option<&TypeDecl>;
}

impl Decls for HashMap<String, TypeDecl> {
    fn decl(&self, name: &str) -> Option<&TypeDecl> {
        self.get(name)
    }
}

impl<T: Decls + ?Sized> Decls for &T {
    fn decl(&self, name: &str) -> Option<&TypeDecl> {
        (**self).decl(name)
    }
}

/// A program's type declarations by name, cloned. A pass that has an
/// [`crate::declared::Owned`] should use its copy.
pub fn decl_map(p: &crate::ast::Program) -> HashMap<String, TypeDecl> {
    let _pp = crate::prof::phase("types::decl_map");
    p.type_decls
        .iter()
        .map(|t| (t.name.clone(), t.clone()))
        .collect()
}

/// Whether a value of type `from` can be used where `to` is expected, without
/// validation. A validated type decays to its base (an `Age` is an `Int64`);
/// the reverse needs [`coercible`].
pub fn assignable(from: &Type, to: &Type, types: &dyn Decls) -> bool {
    assignable_d(from, to, 0, types)
}

const MAX_ASSIGNABLE_DEPTH: usize = 64;

/// [`assignable`] with its descent depth. Comparing two recursive records
/// (`NodeA`, `NodeB`) field by field never ends, so past the cap the answer is
/// "not assignable": a type error instead of a stack overflow.
fn assignable_d(from: &Type, to: &Type, depth: usize, types: &dyn Decls) -> bool {
    if depth > MAX_ASSIGNABLE_DEPTH {
        return false;
    }
    if from == to {
        return true;
    }
    // `Err` is a recovered failure; it fits anything so it causes no second
    // diagnostic.
    if matches!(from, Type::Err) || matches!(to, Type::Err) {
        return true;
    }
    // `Never` is the bottom type, in one direction only.
    if matches!(from, Type::Never) {
        return true;
    }
    // A `lazy T` field takes what a `fn() -> T` field takes; only the read
    // hides the thunk.
    if let Type::Lazy(t) = to {
        return assignable_d(from, &Type::Fn(Vec::new(), t.clone()), depth + 1, types);
    }
    if let Type::Lazy(t) = from {
        return assignable_d(&Type::Fn(Vec::new(), t.clone()), to, depth + 1, types);
    }
    // An alias with no `where` to one of these is interchangeable with its
    // resolved form.
    let transparent = |b: &Type| {
        is_sum_alias(b)
            || matches!(
                b,
                Type::Int
                | Type::IntN { .. }
                | Type::Float
                | Type::Float32
                | Type::Bool
                | Type::Str
                | Type::Map(..)
                | Type::Array(_)
                | Type::ArrayN(..)
                // A named function type is its structural form.
                | Type::Fn(..)
            )
    };
    if let Type::Named(n) = to {
        if let Some(d) = types.decl(n) {
            if d.predicate.is_none() && transparent(&d.base) {
                return assignable_d(from, &d.base, depth + 1, types);
            }
        }
    }
    if let Type::Named(n) = from {
        if let Some(d) = types.decl(n) {
            if d.predicate.is_none() && transparent(&d.base) {
                return assignable_d(&d.base, to, depth + 1, types);
            }
        }
    }
    // A named type decays to its base scalar.
    if let Type::Named(_) = from {
        if matches!(to, Type::Int | Type::Bool | Type::Str) {
            return &resolve(from, types) == to;
        }
    }
    // Covariant: values are immutable.
    if let (Some(a), Some(b)) = (option_payload(from), option_payload(to)) {
        return assignable_d(a, b, depth + 1, types);
    }
    if let (Some((a, e1)), Some((b, e2))) = (result_payloads(from), result_payloads(to)) {
        return assignable_d(a, b, depth + 1, types) && assignable_d(e1, e2, depth + 1, types);
    }
    if let (Type::Map(ka, va), Type::Map(kb, vb)) = (from, to) {
        return assignable_d(ka, kb, depth + 1, types) && assignable_d(va, vb, depth + 1, types);
    }
    if let (Type::Array(a), Type::Array(b)) = (from, to) {
        return assignable_d(a, b, depth + 1, types);
    }
    // Invariant in the capacity `N`.
    if let (Type::SmallArray(a, na), Type::SmallArray(b, nb)) = (from, to) {
        return na == nb && assignable_d(a, b, depth + 1, types);
    }
    // A predicated named type admits only itself.
    if let Type::Named(n) = to {
        if let Some(d) = types.decl(n) {
            if d.predicate.is_some() {
                return matches!(from, Type::Named(m) if m == n);
            }
        }
    }
    // Width subtyping: every field `to` needs, with an assignable type.
    if let (Type::Record(ff), Type::Record(tf)) = (&resolve(from, types), &resolve(to, types)) {
        return tf.iter().all(|need| {
            ff.iter().any(|have| {
                have.name == need.name && assignable_d(&have.ty, &need.ty, depth + 1, types)
            })
        });
    }
    false
}

/// Whether `from` may flow into `to` at a value boundary: [`assignable`], or a
/// value of a predicated type's base, which the boundary then validates
/// (`Checker::prove_coercion` at compile time, else a runtime trap). Top level
/// only: a payload inside `Option`, `Result` or `Array` does not coerce.
pub fn coercible(from: &Type, to: &Type, types: &dyn Decls) -> bool {
    if assignable(from, to, types) {
        return true;
    }
    if let Type::Named(n) = to {
        if let Some(d) = types.decl(n) {
            if d.predicate.is_some() {
                return assignable(from, &d.base, types);
            }
        }
    }
    false
}

/// Reduces `ty` to its structural form: a named type to its base, an
/// application to its substituted base, a transformer to a `Record`, and
/// `lazy T` to `fn() -> T`. An unknown name is `Unit`.
pub fn resolve(ty: &Type, types: &dyn Decls) -> Type {
    resolve_d(ty, types, 0)
}

/// How many `i64` slots a payload of type `t` rides in inside a sum: two for a stored `fn`, a `lazy` and a record of two words,
/// one for anything else, boxed if wider. Structural rather than asking the
/// LLVM shape, which would loop on `type R = { a: Int64, b: Option<R> }`.
pub fn payload_words(ty: &Type, types: &dyn Decls) -> usize {
    let word = |t: &Type| matches!(resolve(t, types), Type::Int | Type::IntN { bits: 64, .. });
    match resolve(ty, types) {
        Type::Fn(..) | Type::Lazy(_) => 2,
        Type::Record(fs) if fs.len() == 2 && fs.iter().all(|f| word(&f.ty)) => 2,
        _ => 1,
    }
}

/// Whether a sum payload of type `t` travels boxed, so the sum owns a heap
/// block per live payload. `Code` answers boxed although the emitter puts it
/// in the word: a release row that frees nothing.
pub fn payload_boxed(ty: &Type, types: &dyn Decls) -> bool {
    if payload_words(ty, types) == 2 {
        return false;
    }
    !matches!(
        resolve(ty, types),
        Type::Int
            | Type::IntN { bits: 64, .. }
            | Type::Bool
            | Type::Str
            | Type::Unit
            | Type::Never
            | Type::Param(_)
    )
}

/// The fields of `ty` if it resolves to a record.
pub fn record_fields(ty: &Type, types: &dyn Decls) -> Option<Vec<Field>> {
    match resolve(ty, types) {
        Type::Record(f) => Some(f),
        _ => None,
    }
}

fn resolve_d(ty: &Type, types: &dyn Decls, depth: usize) -> Type {
    if depth > MAX_DEPTH {
        return Type::Unit;
    }
    match ty {
        // The builtin opaque `Code` resolves to itself; a user `type Code` wins.
        Type::Named(n) if n == "Code" && types.decl("Code").is_none() => {
            Type::Named("Code".to_string())
        }
        // The builtin record `lex()` returns; a user `type Token` wins.
        Type::Named(n) if n == "Token" && types.decl("Token").is_none() => Type::Record(vec![
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
        Type::Named(n) => match types.decl(n) {
            Some(d) => resolve_d(&d.base, types, depth + 1),
            None => Type::Unit,
        },
        Type::App(name, args) => match types.decl(name) {
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
        Type::Partial(base) => match fields_d(base, types, depth) {
            Some(fs) => Type::Record(
                fs.into_iter()
                    .map(|f| Field {
                        name: f.name,
                        ty: Type::option(f.ty),
                    })
                    .collect(),
            ),
            None => Type::Unit,
        },
        // `lazy T` is a stored nullary closure. A record's fields are not
        // resolved, so `Field.ty` keeps the marker for the read, the codec and
        // reflection, which force it.
        Type::Lazy(inner) => Type::Fn(Vec::new(), Box::new(resolve_d(inner, types, depth + 1))),
        other => other.clone(),
    }
}

/// What an `Option<T>` (`| None | Some(T)`) carries. `None` is tag 0.
pub fn option_payload(ty: &Type) -> Option<&Type> {
    match ty {
        Type::Enum(vs) => match vs.as_slice() {
            [n, s] if n.name == "None" && n.payload.is_empty() && s.name == "Some" => {
                s.payload.first()
            }
            _ => None,
        },
        _ => None,
    }
}

/// What a `Result<T, E>` (`| Err(E) | Ok(T)`) carries, as `(T, E)`. `Err` is
/// tag 0.
pub fn result_payloads(ty: &Type) -> Option<(&Type, &Type)> {
    match ty {
        Type::Enum(vs) => match vs.as_slice() {
            [e, o] if e.name == "Err" && o.name == "Ok" => {
                Some((o.payload.first()?, e.payload.first()?))
            }
            _ => None,
        },
        _ => None,
    }
}

/// Whether a declaration's `base` is a built-in sum, as in
/// `type Maybe = Option<Int64>`. The variant names are reserved, so no user
/// enum answers true.
pub fn is_sum_alias(base: &Type) -> bool {
    matches!(base, Type::Enum(vs) if is_builtin_sum(vs))
}

/// Whether a variant list is `Option` or `Result`. The names decide, not the
/// arity: a declared two-variant sum goes through `Fallible` for `?`.
pub fn is_builtin_sum(vs: &[EnumVariant]) -> bool {
    matches!(
        vs,
        [zero, one]
            if (zero.name == "None" && one.name == "Some")
                || (zero.name == "Err" && one.name == "Ok")
    )
}

/// The variants a declaration declares. An alias of a built-in sum declares
/// none, although its `base` is a variant list: collecting `Some` and `Ok` as
/// its own would clash in the name-keyed variant maps.
pub fn declared_variants(base: &Type) -> Option<&[EnumVariant]> {
    match base {
        Type::Enum(vs) if !is_sum_alias(base) => Some(vs),
        _ => None,
    }
}

/// What a `lazy T` field defers, or `None` for a field not forced on read.
pub fn deferred(ty: &Type) -> Option<&Type> {
    match ty {
        Type::Lazy(inner) => Some(inner),
        _ => None,
    }
}

/// A field's type as a read sees it: `lazy T` becomes `T`.
pub fn forced(ty: &Type) -> Type {
    deferred(ty).cloned().unwrap_or_else(|| ty.clone())
}

fn fields_d(ty: &Type, types: &dyn Decls, depth: usize) -> Option<Vec<Field>> {
    match resolve_d(ty, types, depth + 1) {
        Type::Record(f) => Some(f),
        _ => None,
    }
}

/// `fa`'s fields in order, `fb` winning on a name clash, then `fb`'s new ones.
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

/// What a `where` predicate has in scope: every field of a record base by name
/// (`Some(i)` is its index), or the whole value as `value`. The trapping check
/// and the decoder's `validate` both bind exactly this list.
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

    /// [`struct_key`] of a fixed set of types, written down so a build that
    /// renders `Type` differently fails naming the type. If it fails, update
    /// the row only while no artifact outside an emitted module carries a key.
    const PINNED: &[(&str, &str)] = &[
        ("Int64", "0b5f608070c6ce3b"),
        ("Int8", "2682e2651c00bb25"),
        ("UInt8", "6b60ff17449c2bfe"),
        ("String", "8084a51b3c649e88"),
        ("Bool", "49f411f0a1a7f719"),
        ("R", "6dc9d47663615470"),
        ("Option<Int64>", "6f617664e3df370a"),
        ("Array<Int64>", "d9c366f005660b4f"),
        ("Array<Int64, 4>", "789ccf35c7706c73"),
        ("{ a: Int64 }", "9410e46ed45e2516"),
        ("{ b: Int64 }", "b1e8f1e6073bf44e"),
        ("{ a: String }", "71a7ba0548c213ff"),
        ("enum { A }", "4312633fc2f748cb"),
        ("Result<Int64, String>", "56958aa3efc9473c"),
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
            ("Option<Int64>", Type::option(Type::Int)),
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
            ("Result<Int64, String>", Type::result(Type::Int, Type::Str)),
            ("Map<String, Int64>", Type::Map(b(Type::Str), b(Type::Int))),
            ("Box<Int64>", Type::App("Box".into(), vec![Type::Int])),
        ]
    }

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
            // Names the readable mangle collapses with structural types.
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
        assert_eq!(Type::option(Type::Int).to_string(), "Option<Int64>");

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
                    Type::option(t.clone()),
                    Type::Array(b()),
                    Type::ArrayN(b(), 4),
                    Type::ArrayN(b(), 8),
                    Type::SmallArray(b(), 4),
                    Type::Stream(b()),
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
                        Type::result(a.clone(), c.clone()),
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

    #[test]
    fn the_json_codec_names_are_the_shared_identity() {
        let ty = Type::Record(vec![Field {
            name: "a".into(),
            ty: Type::Int,
        }]);
        let k = struct_key(&ty);
        let enc = crate::gen::derived_name(crate::loader::JSON_ENCODERS, &ty);
        let dec = crate::gen::derived_name(crate::loader::JSON_DECODERS, &ty);
        assert!(enc.ends_with(&k));
        assert!(dec.ends_with(&k));
        assert_ne!(enc, dec);
    }
}

#[cfg(test)]
mod json_schema_tests {
    use super::*;

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
    fn negative_bound_is_captured() {
        assert_eq!(
            schema_of("type Temp = Float64 where value >= -273.15", "Temp"),
            "{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\"type\":\"number\",\"minimum\":-273.15}"
        );
    }

    #[test]
    fn negative_integer_bound_is_captured() {
        assert_eq!(
            schema_of("type Debt = Int64 where value >= -5 && value <= 5", "Debt"),
            "{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\"type\":\"integer\",\"minimum\":-5,\"maximum\":5}"
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
    fn string_exclusive_length_floors_to_inclusive() {
        assert_eq!(
            schema_of("type S = String where value.byteLength > 2", "S"),
            "{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\"type\":\"string\",\"minLength\":3}"
        );
    }

    #[test]
    fn exclusive_length_bound_at_the_i64_edge_saturates() {
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
        // The disjunction makes the capture partial: the bound stays and the
        // whole predicate is documented.
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

    /// The strict reader must read back everything `jsonSchema` emits,
    /// control bytes in a pattern included.
    #[test]
    fn a_hostile_pattern_round_trips_through_the_strict_reader() {
        for (src, name) in [
            ("type T = String where value =~ \"a\\rb\"", "T"),
            ("type T = String where value =~ \"a\\tb\\nc\"", "T"),
            // Through the `$comment` path.
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
            crate::schema::parse_json(&s).unwrap_or_else(|e| {
                panic!("strict reader refused its own output ({src}): {e:?}\n{s}")
            });
        }
    }

    #[test]
    fn recursive_record_terminates_with_root_ref() {
        let s = schema_of("type Node = { name: String, next: Option<Node> }", "Node");
        assert!(s.contains("\"next\":{\"$ref\":\"#\"}"), "{s}");
        assert!(s.contains("\"name\":{\"type\":\"string\"}"), "{s}");
    }

    #[test]
    fn mutually_recursive_records_terminate() {
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
        let s = schema_of(
            "type Age = Int64 where value >= 18 \
             type Pair = { x: Age, y: Age }",
            "Pair",
        );
        assert_eq!(s.matches("{\"$ref\":\"#/$defs/Age\"}").count(), 2, "{s}");
        assert_eq!(s.matches("\"minimum\":18").count(), 1, "{s}");
    }

    #[test]
    fn sized_ints_emit_width_bounds() {
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
        assert_eq!(
            schema_of("type Big = UInt64", "Big"),
            "{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\
             \"type\":\"integer\",\"minimum\":0}"
        );
    }

    #[test]
    fn refined_sized_int_merges_bounds_canonically() {
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

/// Binds type parameters by matching a parameter type against a concrete
/// argument type, as the checker's `unify` does without its errors: the same
/// constructor recurses, a different one binds nothing. The outer match has no
/// `_` arm, so a new [`Type`] variant must answer.
pub fn solve_param(pty: &Type, aty: &Type, subst: &mut HashMap<String, Type>) {
    match pty {
        Type::Param(t) => {
            subst.entry(t.clone()).or_insert_with(|| aty.clone());
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
        // An array literal is a fixed `[N x T]` until `coerce` reshapes it,
        // after this solve, so a growable or small parameter binds from one.
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
        // By field name: width subtyping allows extra fields in any order.
        Type::Record(pf) => {
            if let Type::Record(af) = aty {
                for p in pf {
                    if let Some(a) = af.iter().find(|a| a.name == p.name) {
                        solve_param(&p.ty, &a.ty, subst);
                    }
                }
            }
        }
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
        // The argument may arrive resolved, as `fn() -> T`.
        Type::Lazy(p) => match aty {
            Type::Lazy(a) => solve_param(p, a, subst),
            Type::Fn(ap, ar) if ap.is_empty() => solve_param(p, ar, subst),
            _ => {}
        },
        // The checker expands transformers first, so these arms are not taken.
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

#[cfg(test)]
mod sum_alias_tests {
    use super::*;

    fn decl_of(src: &str, name: &str) -> TypeDecl {
        let toks = crate::lexer::lex(src).expect("lex");
        let prog = crate::parser::parse(toks).expect("parse");
        prog.type_decls
            .into_iter()
            .find(|t| t.name == name)
            .expect("declaration")
    }

    #[test]
    fn a_result_alias_declares_no_variants() {
        for src in [
            "export type DeleteResult = Result<Bool, String>",
            "type DeleteResult = Result<Int64, String>",
        ] {
            let d = decl_of(src, "DeleteResult");
            assert!(is_sum_alias(&d.base), "{src}");
            assert!(declared_variants(&d.base).is_none(), "{src}");
        }
    }

    #[test]
    fn a_sum_spelled_as_its_variant_list_declares_no_variants() {
        let types = HashMap::new();
        for ty in [Type::option(Type::Int), Type::result(Type::Bool, Type::Str)] {
            let base = resolve(&ty, &types);
            assert!(matches!(base, Type::Enum(_)), "{ty} resolves to an enum");
            assert!(is_sum_alias(&base), "{ty}");
            assert!(declared_variants(&base).is_none(), "{ty}");
        }
    }

    #[test]
    fn a_declared_enum_still_declares_its_variants() {
        let src = "type Shape =\n    | Circle(Int64)\n    | Rect(Int64, Int64)\n    | Nothing\ntype Lookup = Result<Shape, String>\n";
        let shape = decl_of(src, "Shape");
        assert!(!is_sum_alias(&shape.base));
        let vs = declared_variants(&shape.base).expect("Shape declares variants");
        assert_eq!(
            vs.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(),
            ["Circle", "Rect", "Nothing"]
        );
        let lookup = decl_of(src, "Lookup");
        assert!(is_sum_alias(&lookup.base));
        assert!(declared_variants(&lookup.base).is_none());
    }
}
