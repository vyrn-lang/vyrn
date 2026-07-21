//! Structural type resolution shared by the checker and the codegen backends.
//!
//! `resolve` reduces a [`Type`] to its underlying representation: a validated
//! `Named` type decays to its scalar base, a named record to its `Record`, and
//! the compile-time transformers `Omit`/`Pick`/`Merge` (RFC-0002 §7) evaluate to
//! a concrete `Record`. Transformers are therefore fully erased before codegen.

use std::collections::HashMap;

use crate::ast::*;

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

/// A canonical key for a type used as a protocol-impl target (RFC-0002 §5).
/// Only the types whose runtime value carries enough to dispatch on are
/// supported in v1: the scalars and named types (validated scalars, enums).
/// Records and other structural types return `None` (no runtime identity).
pub fn type_key(ty: &Type) -> Option<String> {
    match ty {
        Type::Int => Some("Int64".to_string()),
        Type::Bool => Some("Bool".to_string()),
        Type::Str => Some("String".to_string()),
        Type::Named(n) => Some(n.clone()),
        _ => None,
    }
}

/// The internal (mangled) function name for an `impl` method, e.g.
/// `Show__Int__show`. Unique per (protocol, target type, method).
pub fn impl_method_name(protocol: &str, type_key: &str, method: &str) -> String {
    format!("{protocol}__{type_key}__{method}")
}

/// Extract the `(min, max)` inclusive numeric bounds a validated type's `where`
/// predicate implies (RFC-0003 reflection). Recognizes `value >=/> N`,
/// `value <=/< N` in either operand order, and `&&` conjunctions. Anything else
/// (e.g. `value % 2 == 0`) contributes no bound.
pub fn predicate_bounds(pred: &Expr) -> (Option<i64>, Option<i64>) {
    if let Expr::Binary { op, lhs, rhs, .. } = pred {
        if *op == BinOp::And {
            let (l0, l1) = predicate_bounds(lhs);
            let (r0, r1) = predicate_bounds(rhs);
            return (l0.or(r0), l1.or(r1));
        }
        // `value OP n` or `n OP value` → normalize to `value OP n`.
        let (normalized, n) = match (&**lhs, &**rhs) {
            (Expr::Var { name, .. }, Expr::Int(n)) if name == "value" => (*op, *n),
            (Expr::Int(n), Expr::Var { name, .. }) if name == "value" => (flip(*op), *n),
            _ => return (None, None),
        };
        return match normalized {
            BinOp::GtEq => (Some(n), None),
            BinOp::Gt => (Some(n + 1), None),
            BinOp::LtEq => (None, Some(n)),
            BinOp::Lt => (None, Some(n - 1)),
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
/// `value.length OP N` comparisons (exclusive bounds are floored/ceiled to
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
                BinOp::Gt => (Some(n + 1), None),
                BinOp::LtEq => (None, Some(n)),
                BinOp::Lt => (None, Some(n - 1)),
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
/// `$schema` dialect. Recurses through records, arrays, and options. A named
/// type renders as `{"$ref":"#/$defs/N"}` (the root itself as `{"$ref":"#"}`)
/// with its schema collected into [`SchemaCx::defs`] — except synthetic inline
/// refinement helpers (`User.age`), which stay inlined so a field-level `where`
/// round-trips as the inline constraints the user wrote.
fn type_schema(ty: &Type, cx: &mut SchemaCx) -> String {
    match ty {
        // The default Int64 is "just an integer"; a *sized* int is a deliberate
        // wire-width choice, so its bounds are part of the contract.
        Type::Int => "{\"type\":\"integer\"}".to_string(),
        Type::IntN { bits, signed } => intn_schema(*bits, *signed, &[]),
        Type::Float | Type::Float32 => "{\"type\":\"number\"}".to_string(),
        Type::Bool => "{\"type\":\"boolean\"}".to_string(),
        Type::Str => "{\"type\":\"string\"}".to_string(),
        // An `Option<T>` field carries `T`'s schema; its optionality is expressed
        // by omission from the enclosing object's `required` list.
        Type::Option(inner) => type_schema(inner, cx),
        Type::Array(inner) | Type::ArrayN(inner, _) => {
            format!(
                "{{\"type\":\"array\",\"items\":{}}}",
                type_schema(inner, cx)
            )
        }
        // A `Map<String, V>` (RFC-0028) is a free-form JSON object whose values
        // all share `V`'s schema: `additionalProperties` carries it.
        Type::Map(_, val) => {
            format!(
                "{{\"type\":\"object\",\"additionalProperties\":{}}}",
                type_schema(val, cx)
            )
        }
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
        Type::Record(fields) => record_schema(fields, cx),
        // A payload-less sum type is exactly a JSON Schema `enum` of its variant
        // names (unchanged). A payload sum type emits the RFC-0024 externally-
        // tagged `oneOf`, which the importer recognizes back into an enum decl.
        Type::Enum(variants) => {
            if variants.iter().all(|v| v.payload.is_empty()) {
                let names: Vec<String> = variants
                    .iter()
                    .map(|v| format!("\"{}\"", json_escape(&v.name)))
                    .collect();
                format!("{{\"enum\":[{}]}}", names.join(","))
            } else {
                enum_oneof_schema(variants, cx)
            }
        }
        // `Result<T, E>` on the wire is a two-variant single-payload enum
        // (`{"Ok":<T>}` / `{"Err":<E>}`) — emit the identical `oneOf` (RFC-0024).
        Type::Result(t, e) => {
            let variants = vec![
                EnumVariant {
                    name: "Ok".to_string(),
                    payload: vec![(**t).clone()],
                },
                EnumVariant {
                    name: "Err".to_string(),
                    payload: vec![(**e).clone()],
                },
            ];
            enum_oneof_schema(&variants, cx)
        }
        _ => "{}".to_string(),
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
/// bounds via `value.length OP N` (→ `minLength`/`maxLength`) and `value =~ "…"`
/// (→ `pattern`). Two or more patterns are combined with `allOf` (a JSON object
/// has at most one `pattern`). A form the model can't capture is documented in a
/// `$comment` (as for scalars).
///
/// Length semantics (decided): Vyrn's `value.length` counts **bytes** (native
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

/// Collect `minLength`/`maxLength` from a `String` predicate over `value.length`,
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
    // `value.length OP N` or `N OP value.length`. `>` and `>=` both floor the
    // length (JSON Schema minLength is inclusive), so `> N` becomes `N + 1`.
    let (norm, lit) = match (&**lhs, &**rhs) {
        (l, r) if is_length_of_value(l) => (*op, int_lit(r)),
        (l, r) if is_length_of_value(r) => (flip(*op), int_lit(l)),
        _ => return false,
    };
    match (norm, lit) {
        (BinOp::GtEq, Some(n)) => push_true(out, "minLength", n.to_string()),
        (BinOp::Gt, Some(n)) => push_true(out, "minLength", (n + 1).to_string()),
        (BinOp::LtEq, Some(n)) => push_true(out, "maxLength", n.to_string()),
        (BinOp::Lt, Some(n)) => push_true(out, "maxLength", (n - 1).to_string()),
        _ => false,
    }
}

/// True if `e` is `value.length`.
fn is_length_of_value(e: &Expr) -> bool {
    matches!(e, Expr::Field { expr, field, .. } if field == "length" && is_value(expr))
}

/// An integer literal (possibly negated) as an `i64`, or `None`.
fn int_lit(e: &Expr) -> Option<i64> {
    match e {
        Expr::Int(n) => Some(*n),
        Expr::Unary {
            op: UnOp::Neg,
            expr,
            ..
        } => match &**expr {
            Expr::Int(n) => Some(-n),
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
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

/// A record maps to a JSON Schema `object`; non-`Option` fields are `required`.
fn record_schema(fields: &[Field], cx: &mut SchemaCx) -> String {
    let props: Vec<String> = fields
        .iter()
        .map(|f| format!("\"{}\":{}", f.name, type_schema(&f.ty, cx)))
        .collect();
    let required: Vec<String> = fields
        .iter()
        .filter(|f| !matches!(f.ty, Type::Option(_)))
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
        Type::Ref(inner) => Type::Ref(Box::new(substitute(inner, subst))),
        Type::Array(inner) => Type::Array(Box::new(substitute(inner, subst))),
        Type::ArrayN(inner, n) => Type::ArrayN(Box::new(substitute(inner, subst)), *n),
        Type::Map(k, v) => Type::Map(
            Box::new(substitute(k, subst)),
            Box::new(substitute(v, subst)),
        ),
        Type::Task(inner) => Type::Task(Box::new(substitute(inner, subst))),
        // A function-value type (RFC-0023): substitute into its parameter and
        // return types so a generic `fn(T) -> U` monomorphizes with the rest.
        Type::Fn(params, ret) => Type::Fn(
            params.iter().map(|p| substitute(p, subst)).collect(),
            Box::new(substitute(ret, subst)),
        ),
        other => other.clone(),
    }
}

/// Reduce `ty` to its structural form (scalar, `Record`, `Option`, `Result`, …).
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
        other => other.clone(),
    }
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
            schema_of("type Username = String where value.length >= 3 && value.length <= 16", "Username"),
            "{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\"type\":\"string\",\"minLength\":3,\"maxLength\":16}"
        );
    }

    #[test]
    fn string_exclusive_length_floors_to_inclusive() {
        // `value.length > 2` ⇒ minLength 3 (JSON Schema minLength is inclusive).
        assert_eq!(
            schema_of("type S = String where value.length > 2", "S"),
            "{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\"type\":\"string\",\"minLength\":3}"
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

    #[test]
    fn multiple_patterns_combine_with_allof() {
        // Size + two regex clauses: length maps directly, the patterns go in `allOf`
        // (a JSON object permits only one `pattern`).
        let s = schema_of(
            "type W = String where value.length >= 4 && value =~ \"[a-z]+\" && value =~ \"(.a)*\"",
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
            "type User = { name: String where value.length >= 3, \
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
