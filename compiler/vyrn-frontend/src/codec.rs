//! The JSON codec (RFC-0018): the shared, backend-neutral heart of
//! `toJson` / `fromJson`.
//!
//! This module owns three things that MUST be identical across the interpreter,
//! the native backend, and wasm:
//!
//! 1. **Codability** — which types may cross the wire ([`encodable`] /
//!    [`decodable`]), rejecting the non-codable with the offender named.
//! 2. **The exact-integer JSON parser** ([`parse`]) — a sibling of the
//!    order-preserving parser in [`crate::schema`], but one that keeps integers
//!    *exact* (never through `f64`) by remembering each number's source text and
//!    whether it was written in integer syntax. The native side mirrors this
//!    parser byte-for-byte in the C runtime shim (see `vyrn-cli`), including the
//!    error wording ([`ParseError`]).
//! 3. **The Issue vocabulary** — the exact `key`/`message` bytes for every
//!    decode failure ([`type_message`], [`missing_message`], [`validate_message`],
//!    and the `parse` wording). Every message except a parse error is a
//!    *compile-time constant* per type/site, so both backends bake the identical
//!    string; only parse errors carry a runtime byte position, formatted the
//!    same way on both sides.
//!
//! The locked decode semantics (RFC-0018): unknown JSON fields are ignored,
//! `Option<T>` accepts absent OR `null` → `None`, integers parse exactly, and
//! every `where` clause runs — failures **accumulate** as `Issue`s rather than
//! trapping.

use crate::ast::*;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// exact-integer JSON value
// ---------------------------------------------------------------------------

/// A parsed JSON value that keeps integers exact. Unlike [`crate::schema::Json`]
/// (which stores every number as `f64`), a [`Num`] remembers its source text and
/// whether it used integer syntax, so an `Int64`/sized-int target can be decoded
/// without ever routing a 53-bit-limited `f64` in between.
#[derive(Debug, Clone, PartialEq)]
pub enum JsonV {
    Null,
    Bool(bool),
    Num(Num),
    Str(String),
    Arr(Vec<JsonV>),
    /// Insertion-ordered object (unknown fields are ignored at decode, but the
    /// order is preserved so diagnostics are deterministic).
    Obj(Vec<(String, JsonV)>),
}

/// A JSON number token, kept as text for exact re-parsing.
#[derive(Debug, Clone, PartialEq)]
pub struct Num {
    /// The verbatim source text (e.g. `-9007199254740993`, `1.5`, `2e3`).
    pub text: String,
    /// True when the token had no `.`/`e`/`E` — i.e. it was written as an
    /// integer. Only integer-syntax numbers may decode into an integer target.
    pub is_int: bool,
}

impl Num {
    /// The exact `i64` value, or `None` when the token is not integer syntax or
    /// does not fit in `i64`.
    pub fn as_i64(&self) -> Option<i64> {
        if !self.is_int {
            return None;
        }
        self.text.parse::<i64>().ok()
    }
    /// The value as `f64` (for `Float`/`Float32` targets).
    pub fn as_f64(&self) -> f64 {
        self.text.parse::<f64>().unwrap_or(f64::NAN)
    }
}

impl JsonV {
    /// The JSON kind name used in `expected <X>, found <kind>` messages.
    pub fn kind(&self) -> &'static str {
        match self {
            JsonV::Null => "null",
            JsonV::Bool(_) => "boolean",
            JsonV::Num(_) => "number",
            JsonV::Str(_) => "string",
            JsonV::Arr(_) => "array",
            JsonV::Obj(_) => "object",
        }
    }
    pub fn get(&self, key: &str) -> Option<&JsonV> {
        match self {
            JsonV::Obj(fs) => fs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// parser (byte positions; wording mirrored by the C runtime shim)
// ---------------------------------------------------------------------------

/// A parse failure, carrying the exact `json.parse` message bytes. The C shim
/// (`__vyrn_json_parse`) produces the identical strings.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError(pub String);

/// Parse `src` into a [`JsonV`], keeping integers exact. Byte positions are
/// 0-based offsets into the UTF-8 source. The error wording is part of the
/// parity surface — keep it in lockstep with the C shim.
pub fn parse(src: &str) -> Result<JsonV, ParseError> {
    let b = src.as_bytes();
    let mut p = Parser { b, i: 0 };
    p.ws();
    let v = p.value()?;
    p.ws();
    if p.i != b.len() {
        return Err(ParseError(format!(
            "trailing characters at position {}",
            p.i
        )));
    }
    Ok(v)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() {
            match self.b[self.i] {
                b' ' | b'\t' | b'\n' | b'\r' => self.i += 1,
                _ => break,
            }
        }
    }
    fn eoi(&self) -> ParseError {
        ParseError("unexpected end of input".to_string())
    }
    fn unexpected(&self) -> ParseError {
        ParseError(format!("unexpected character at position {}", self.i))
    }
    fn value(&mut self) -> Result<JsonV, ParseError> {
        match self.b.get(self.i) {
            None => Err(self.eoi()),
            Some(b'{') => self.obj(),
            Some(b'[') => self.arr(),
            Some(b'"') => Ok(JsonV::Str(self.string()?)),
            Some(b't') => self.lit("true", JsonV::Bool(true)),
            Some(b'f') => self.lit("false", JsonV::Bool(false)),
            Some(b'n') => self.lit("null", JsonV::Null),
            Some(c) if *c == b'-' || c.is_ascii_digit() => self.num(),
            Some(_) => Err(self.unexpected()),
        }
    }
    fn lit(&mut self, word: &str, v: JsonV) -> Result<JsonV, ParseError> {
        for &wb in word.as_bytes() {
            match self.b.get(self.i) {
                None => return Err(self.eoi()),
                Some(c) if *c == wb => self.i += 1,
                Some(_) => return Err(self.unexpected()),
            }
        }
        Ok(v)
    }
    fn obj(&mut self) -> Result<JsonV, ParseError> {
        self.i += 1; // '{'
        let mut out = Vec::new();
        self.ws();
        if self.b.get(self.i) == Some(&b'}') {
            self.i += 1;
            return Ok(JsonV::Obj(out));
        }
        loop {
            self.ws();
            if self.b.get(self.i) != Some(&b'"') {
                return Err(if self.i >= self.b.len() {
                    self.eoi()
                } else {
                    self.unexpected()
                });
            }
            let k = self.string()?;
            self.ws();
            if self.b.get(self.i) != Some(&b':') {
                return Err(if self.i >= self.b.len() {
                    self.eoi()
                } else {
                    self.unexpected()
                });
            }
            self.i += 1;
            self.ws();
            let v = self.value()?;
            out.push((k, v));
            self.ws();
            match self.b.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    return Ok(JsonV::Obj(out));
                }
                None => return Err(self.eoi()),
                Some(_) => return Err(self.unexpected()),
            }
        }
    }
    fn arr(&mut self) -> Result<JsonV, ParseError> {
        self.i += 1; // '['
        let mut out = Vec::new();
        self.ws();
        if self.b.get(self.i) == Some(&b']') {
            self.i += 1;
            return Ok(JsonV::Arr(out));
        }
        loop {
            self.ws();
            out.push(self.value()?);
            self.ws();
            match self.b.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    return Ok(JsonV::Arr(out));
                }
                None => return Err(self.eoi()),
                Some(_) => return Err(self.unexpected()),
            }
        }
    }
    fn string(&mut self) -> Result<String, ParseError> {
        self.i += 1; // opening quote
        let mut out = String::new();
        loop {
            match self.b.get(self.i) {
                None => return Err(self.eoi()),
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.i += 1;
                    match self.b.get(self.i) {
                        None => return Err(self.eoi()),
                        Some(b'"') => out.push('"'),
                        Some(b'\\') => out.push('\\'),
                        Some(b'/') => out.push('/'),
                        Some(b'n') => out.push('\n'),
                        Some(b't') => out.push('\t'),
                        Some(b'r') => out.push('\r'),
                        Some(b'b') => out.push('\u{8}'),
                        Some(b'f') => out.push('\u{c}'),
                        Some(b'u') => {
                            let mut cp: u32 = 0;
                            for _ in 0..4 {
                                self.i += 1;
                                let h = match self.b.get(self.i) {
                                    None => return Err(self.eoi()),
                                    Some(c) => hex_digit(*c).ok_or_else(|| self.unexpected())?,
                                };
                                cp = cp * 16 + h as u32;
                            }
                            match char::from_u32(cp) {
                                Some(ch) => out.push(ch),
                                None => return Err(self.unexpected()),
                            }
                        }
                        Some(_) => return Err(self.unexpected()),
                    }
                    self.i += 1;
                }
                // A raw control byte (< 0x20) is invalid in a JSON string.
                Some(c) if *c < 0x20 => return Err(self.unexpected()),
                Some(_) => {
                    // Copy one UTF-8 codepoint's bytes through verbatim.
                    let start = self.i;
                    self.i += 1;
                    while self.i < self.b.len() && (self.b[self.i] & 0xC0) == 0x80 {
                        self.i += 1;
                    }
                    out.push_str(std::str::from_utf8(&self.b[start..self.i]).unwrap_or("\u{fffd}"));
                }
            }
        }
    }
    fn num(&mut self) -> Result<JsonV, ParseError> {
        let start = self.i;
        let mut is_int = true;
        if self.b.get(self.i) == Some(&b'-') {
            self.i += 1;
        }
        // integer part
        if !matches!(self.b.get(self.i), Some(c) if c.is_ascii_digit()) {
            return Err(if self.i >= self.b.len() {
                self.eoi()
            } else {
                self.unexpected()
            });
        }
        while matches!(self.b.get(self.i), Some(c) if c.is_ascii_digit()) {
            self.i += 1;
        }
        // fraction
        if self.b.get(self.i) == Some(&b'.') {
            is_int = false;
            self.i += 1;
            if !matches!(self.b.get(self.i), Some(c) if c.is_ascii_digit()) {
                return Err(if self.i >= self.b.len() {
                    self.eoi()
                } else {
                    self.unexpected()
                });
            }
            while matches!(self.b.get(self.i), Some(c) if c.is_ascii_digit()) {
                self.i += 1;
            }
        }
        // exponent
        if matches!(self.b.get(self.i), Some(b'e') | Some(b'E')) {
            is_int = false;
            self.i += 1;
            if matches!(self.b.get(self.i), Some(b'+') | Some(b'-')) {
                self.i += 1;
            }
            if !matches!(self.b.get(self.i), Some(c) if c.is_ascii_digit()) {
                return Err(if self.i >= self.b.len() {
                    self.eoi()
                } else {
                    self.unexpected()
                });
            }
            while matches!(self.b.get(self.i), Some(c) if c.is_ascii_digit()) {
                self.i += 1;
            }
        }
        let text = String::from_utf8_lossy(&self.b[start..self.i]).into_owned();
        Ok(JsonV::Num(Num { text, is_int }))
    }
}

fn hex_digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// canonical string escaping (encode side)
// ---------------------------------------------------------------------------

/// Escape a string into a JSON string body (WITHOUT the surrounding quotes),
/// using the minimal RFC-0018 table: `\" \\ \n \t \r`, `\u00XX` for other
/// control bytes, everything else verbatim. Both backends must produce these
/// exact bytes.
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

// ---------------------------------------------------------------------------
// Issue vocabulary (locked wording — shared by both backends)
// ---------------------------------------------------------------------------

/// The `expected <what>` phrase for a decode target's resolved structural type.
pub fn expected_name(ty: &Type, types: &HashMap<String, TypeDecl>) -> String {
    match crate::types::resolve(ty, types) {
        Type::Record(_) => "object".to_string(),
        Type::Map(..) => "object".to_string(),
        Type::Array(_) | Type::ArrayN(..) => "array".to_string(),
        Type::Str => "string".to_string(),
        Type::Int | Type::IntN { .. } => "integer".to_string(),
        Type::Float | Type::Float32 => "number".to_string(),
        Type::Bool => "boolean".to_string(),
        Type::Enum(vs) => enum_expected(&vs),
        Type::Result(..) => result_expected(),
        _ => "value".to_string(),
    }
}

/// `one of \`A\`, \`B\`` for an enum target — lists every variant name (payload
/// or not), the uniformity rule for the `json.type` expected-one-of message.
pub fn enum_expected(vs: &[EnumVariant]) -> String {
    let names: Vec<String> = vs.iter().map(|v| format!("`{}`", v.name)).collect();
    format!("one of {}", names.join(", "))
}

/// `one of \`Ok\`, \`Err\`` for a `Result<T, E>` decode target.
pub fn result_expected() -> String {
    "one of `Ok`, `Err`".to_string()
}

/// `json.type` message: `expected <what>, found <kind>`.
pub fn type_message(expected: &str, found: &str) -> String {
    format!("expected {expected}, found {found}")
}

/// `json.missing` message: ``missing required field `name` ``.
pub fn missing_message(field: &str) -> String {
    format!("missing required field `{field}`")
}

/// `validate` message — the canonical validation wording for a refined type,
/// byte-identical to the trap the interpreter/codegen raise at other
/// boundaries (see `interp::coerce` / codegen `emit`), only accumulated as an
/// Issue instead of trapping.
pub fn validate_message(decl: &TypeDecl) -> String {
    if matches!(decl.base, Type::Record(_)) {
        format!(
            "validation failed: `{}` violates its `where` clause",
            decl.name
        )
    } else {
        format!("validation failed for `{}`", decl.name)
    }
}

/// Extend a dotted/indexed path with a record field.
pub fn field_path(parent: &str, field: &str) -> String {
    if parent.is_empty() {
        field.to_string()
    } else {
        format!("{parent}.{field}")
    }
}

/// Extend a dotted/indexed path with an array index.
pub fn index_path(parent: &str, i: usize) -> String {
    format!("{parent}[{i}]")
}

// ---------------------------------------------------------------------------
// codability
// ---------------------------------------------------------------------------

/// Whether `ty` may be **encoded** by `toJson` (the encode domain is slightly
/// wider than decode: a fixed `Array<T, N>` encodes as an ordinary array).
/// Returns `Err(offender)` naming the first non-codable type otherwise.
pub fn encodable(ty: &Type, types: &HashMap<String, TypeDecl>) -> Result<(), String> {
    codable(ty, types, false, &mut Vec::new())
}

/// Whether `ty` may be a **decode target** for `fromJson`. Stricter than
/// [`encodable`]: an `Array<T, N>` cannot be a decode target (its length is not
/// known until the data arrives).
pub fn decodable(ty: &Type, types: &HashMap<String, TypeDecl>) -> Result<(), String> {
    codable(ty, types, true, &mut Vec::new())
}

fn codable(
    ty: &Type,
    types: &HashMap<String, TypeDecl>,
    decode: bool,
    seen: &mut Vec<String>,
) -> Result<(), String> {
    // A named type is described by the *spelling the user wrote* in error
    // messages, but we recurse through its structural form.
    let display = type_display(ty);
    // `Validation<T>` stays off the wire in v1 (its `Invalid` carries an
    // `Array<Issue>` and its `Valid` a generic payload) — an explicit reject so
    // the diagnostic names `Validation` rather than a generic-parameter leaf
    // (RFC-0024 out-of-scope; a one-line follow-up if ever wanted).
    if is_validation(ty) {
        return Err("Validation".to_string());
    }
    match ty {
        Type::Int | Type::IntN { .. } | Type::Float | Type::Float32 | Type::Bool | Type::Str => {
            Ok(())
        }
        Type::Option(inner) => {
            // A nested Option is a decode hazard (double `null`), so name it here.
            // An `Option<Result<..>>` / `Option<Enum>` IS codable (RFC-0024): a
            // payload enum/Result never encodes as `null`, so the wire form stays
            // unambiguous.
            if matches!(**inner, Type::Option(_)) {
                return Err(display);
            }
            codable(inner, types, decode, seen)
        }
        Type::Array(inner) => codable(inner, types, decode, seen),
        // A `Map<String, V>` (RFC-0028) is a JSON object; codable when `V` is.
        // The key is always `String` (the checker enforces it), so only the
        // value type is checked.
        Type::Map(_, val) => codable(val, types, decode, seen),
        Type::ArrayN(inner, _) => {
            if decode {
                Err(display)
            } else {
                codable(inner, types, decode, seen)
            }
        }
        Type::Record(fields) => {
            for f in fields {
                codable(&f.ty, types, decode, seen)?;
            }
            Ok(())
        }
        // A payload enum is codable when every variant's payloads are (RFC-0024).
        // A rejection names the offending variant + payload type so the diagnostic
        // is precise (`Task<Int64> (payload of variant \`Boxed\`)`).
        Type::Enum(vs) => enum_codable(vs, types, decode, seen),
        // `Result<T, E>` flows through as a two-variant payload enum
        // (`{"Ok":<T>}` / `{"Err":<E>}`): codable when both payloads are.
        Type::Result(t, e) => {
            codable(t, types, decode, seen).map_err(|_| enum_payload_offender(t, "Ok"))?;
            codable(e, types, decode, seen).map_err(|_| enum_payload_offender(e, "Err"))?;
            Ok(())
        }
        Type::Named(n) => {
            if seen.iter().any(|s| s == n) {
                return Ok(()); // already being checked — break the cycle
            }
            match types.get(n) {
                None => Err(n.clone()),
                Some(d) => {
                    seen.push(n.clone());
                    let r = codable(&d.base, types, decode, seen);
                    seen.pop();
                    // Preserve a payload-enum's precise variant/payload offender;
                    // re-badge any other structural rejection with the user's name.
                    r.map_err(|e| {
                        if matches!(&d.base, Type::Enum(vs) if vs.iter().any(|v| !v.payload.is_empty()))
                            || matches!(d.base, Type::Result(..))
                        {
                            e
                        } else {
                            n.clone()
                        }
                    })
                }
            }
        }
        // Everything else is off the wire in v1: Ref, Task, Template, Logger,
        // type transformers, Param, Unit, Err.
        _ => Err(display),
    }
}

/// Whether `ty` is a `Validation<..>` (either the bare `Named`/`App` spelling or
/// a resolved enum with the built-in `Valid`/`Invalid` variants).
fn is_validation(ty: &Type) -> bool {
    match ty {
        Type::Named(n) => n == "Validation",
        Type::App(n, _) => n == "Validation",
        _ => false,
    }
}

/// A payload enum is codable when every variant's payloads are. The error names
/// the first offending variant + payload type.
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

/// The rejection message for a non-codable enum payload: `<type> (payload of
/// variant \`Name\`)`.
fn enum_payload_offender(p: &Type, variant: &str) -> String {
    format!("{} (payload of variant `{}`)", type_display(p), variant)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `Map<String, V>` is codable exactly when `V` is (RFC-0028): the key is
    /// always `String`, so only the value type gates codability.
    #[test]
    fn map_codability_follows_the_value_type() {
        let types = HashMap::new();
        let ok = Type::Map(Box::new(Type::Str), Box::new(Type::Int));
        assert!(encodable(&ok, &types).is_ok());
        assert!(decodable(&ok, &types).is_ok());

        // Nested: Map<String, Array<Int64>> is codable.
        let nested = Type::Map(
            Box::new(Type::Str),
            Box::new(Type::Array(Box::new(Type::Int))),
        );
        assert!(encodable(&nested, &types).is_ok());
        assert!(decodable(&nested, &types).is_ok());

        // A non-codable value type (a `Ref`) makes the whole map non-codable,
        // and the offender is named.
        let bad = Type::Map(
            Box::new(Type::Str),
            Box::new(Type::Ref(Box::new(Type::Int))),
        );
        assert_eq!(encodable(&bad, &types).unwrap_err(), "Ref");
    }

    /// The decode-side `expected` phrase for a map value is `object` (a Map IS a
    /// JSON object) — the wording that lands in a `json.type` Issue.
    #[test]
    fn map_expected_name_is_object() {
        let types = HashMap::new();
        let m = Type::Map(Box::new(Type::Str), Box::new(Type::Int));
        assert_eq!(expected_name(&m, &types), "object");
    }
}

/// A user-facing spelling for a type, for the codability rejection message.
fn type_display(ty: &Type) -> String {
    match ty {
        Type::Named(n) => n.clone(),
        Type::Result(..) => "Result".to_string(),
        Type::Ref(_) => "Ref".to_string(),
        Type::Task(_) => "Task".to_string(),
        Type::Logger => "Logger".to_string(),
        Type::ArrayN(inner, n) => format!("Array<{}, {}>", type_display(inner), n),
        Type::Option(inner) => format!("Option<{}>", type_display(inner)),
        Type::Unit => "Unit".to_string(),
        other => format!("{other}"),
    }
}
