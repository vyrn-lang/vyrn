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
    let mut p = Parser { b, i: 0, depth: 0 };
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
    /// Objects and arrays currently open — the recursion this parser bounds,
    /// at the same depth [`crate::schema`] bounds its own recursive-descent
    /// parser (`MAX_JSON_DEPTH`): a document deeper than that is refused with
    /// a named error instead of taking the thread stack down.
    depth: usize,
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
            Some(b'{') => {
                self.nest()?;
                let v = self.obj();
                self.depth -= 1;
                v
            }
            Some(b'[') => {
                self.nest()?;
                let v = self.arr();
                self.depth -= 1;
                v
            }
            Some(b'"') => Ok(JsonV::Str(self.string()?)),
            Some(b't') => self.lit("true", JsonV::Bool(true)),
            Some(b'f') => self.lit("false", JsonV::Bool(false)),
            Some(b'n') => self.lit("null", JsonV::Null),
            Some(c) if *c == b'-' || c.is_ascii_digit() => self.num(),
            Some(_) => Err(self.unexpected()),
        }
    }
    /// Enter one enclosing level, or refuse. Every recursive call goes through
    /// here, so the bound is the parser's and not one caller's.
    fn nest(&mut self) -> Result<(), ParseError> {
        self.depth += 1;
        if self.depth > crate::schema::MAX_JSON_DEPTH {
            return Err(ParseError(format!(
                "nested deeper than {} levels at position {}",
                crate::schema::MAX_JSON_DEPTH,
                self.i
            )));
        }
        Ok(())
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
            // A name defined twice is a document with two meanings, and this
            // parser's role is to agree with the shipped reader: `std/jsonread`
            // and [`crate::schema`] refuse a duplicate outright, so first-wins
            // `JsonV::get` never gets the chance to hide one.
            if out.iter().any(|(prev, _)| prev == &k) {
                return Err(ParseError(format!(
                    "`{k}` is defined twice at position {}",
                    self.i
                )));
            }
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
                            // A surrogate half is not a scalar. A high half
                            // (D800–DBFF) followed immediately by its low
                            // escape (DC00–DFFF) combines into one scalar —
                            // how RFC 8259 §7 spells an astral codepoint, and
                            // what `std/jsonread` accepts — while a lone or
                            // unpaired half is refused with the parity wording.
                            let ch = if (0xd800..=0xdbff).contains(&cp) {
                                if self.b.get(self.i + 1) != Some(&b'\\')
                                    || self.b.get(self.i + 2) != Some(&b'u')
                                {
                                    return Err(self.unexpected());
                                }
                                self.i += 2;
                                let mut lo: u32 = 0;
                                for _ in 0..4 {
                                    self.i += 1;
                                    let h = match self.b.get(self.i) {
                                        None => return Err(self.eoi()),
                                        Some(c) => {
                                            hex_digit(*c).ok_or_else(|| self.unexpected())?
                                        }
                                    };
                                    lo = lo * 16 + h as u32;
                                }
                                if !(0xdc00..=0xdfff).contains(&lo) {
                                    return Err(self.unexpected());
                                }
                                char::from_u32(0x10000 + (cp - 0xd800) * 0x400 + (lo - 0xdc00))
                                    .unwrap()
                            } else {
                                char::from_u32(cp).ok_or_else(|| self.unexpected())?
                            };
                            out.push(ch);
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
        // integer part — the JSON grammar's `0 | [1-9][0-9]*`: a leading zero
        // (`01`) is two tokens to every strict reader, so it is refused here
        // rather than silently read as one number.
        match self.b.get(self.i) {
            None => return Err(self.eoi()),
            Some(c) if c.is_ascii_digit() => self.i += 1,
            Some(_) => return Err(self.unexpected()),
        }
        if self.b[self.i - 1] == b'0' && matches!(self.b.get(self.i), Some(c) if c.is_ascii_digit())
        {
            return Err(self.unexpected());
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

/// The `expected <what>` phrase for a decode target — the JSON kind its [`Wire`]
/// form arrives as. Asked in the encode direction because it names what the
/// *data* looks like, which both directions agree about.
pub fn expected_name(ty: &Type, types: &HashMap<String, TypeDecl>) -> String {
    match wire(ty, types, false) {
        Ok(Wire::Record(_) | Wire::Map(_)) => "object".to_string(),
        Ok(Wire::Array(_) | Wire::FixedArray(..)) => "array".to_string(),
        Ok(Wire::Str) => "string".to_string(),
        Ok(Wire::Int | Wire::IntN { .. }) => "integer".to_string(),
        Ok(Wire::Float | Wire::Float32) => "number".to_string(),
        Ok(Wire::Bool) => "boolean".to_string(),
        // A `Result<T, E>` is a two-variant enum on the wire, so this is where
        // its `one of \`Ok\`, \`Err\`` comes from — one wording, not two.
        Ok(Wire::Enum(vs)) => enum_expected(&vs),
        Ok(Wire::Option(_)) | Err(_) => "value".to_string(),
    }
}

/// `one of \`A\`, \`B\`` for an enum target — lists every variant name (payload
/// or not), the uniformity rule for the `json.type` expected-one-of message.
pub fn enum_expected(vs: &[EnumVariant]) -> String {
    let names: Vec<String> = vs.iter().map(|v| format!("`{}`", v.name)).collect();
    format!("one of {}", names.join(", "))
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
    crate::trap::validation_of(decl)
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
// the wire form
// ---------------------------------------------------------------------------

/// What a type **is on the JSON wire**, once names, generic applications and
/// record transformers are resolved away.
///
/// One answer, because there used to be four. "Which types cross the wire, and
/// as what" was decided independently by [`codable`] (the checker's gate),
/// `types::type_schema` (the schema emitter), `jsonenc::Walk::body` (the encoder
/// synthesizer) and `jsondec::Walk::body` (the decoder synthesizer) — four
/// `match`es over [`Type`], three of them with an open `_` arm. They had already
/// drifted, and the disagreement ran the same way every time: the two
/// SYNTHESIZERS call [`crate::types::resolve`] first, and the two others matched
/// the raw spelling, so `Omit<User, password>`, `Pick`, `Merge`, `Partial` and
/// every applied generic (`Box<Int64>`) were rejected by the gate, described as
/// `{}` ("anything") by the schema, and encoded/decoded perfectly by the two
/// passes that never got asked.
///
/// Resolving is the correct answer of the three. A record transformer IS the
/// record it computes — that is what RFC-0002 §7 defines it as, and what every
/// backend lowers it to — so refusing to encode one refuses the exact use the
/// feature exists for (`toJson(Omit<User, password>)`), and describing it as
/// "anything" is a schema that says nothing about a shape that is fully known.
///
/// # What this cannot answer, and why each site keeps a step of its own
///
/// A [`Type::Named`] is handled by its caller BEFORE it gets here, because the
/// three callers that care break the same cycle three different ways and none of
/// them is the others': [`codable`] carries a `seen` list to make a
/// self-referential type terminate, `type_schema` emits `{"$ref":"#/$defs/N"}`
/// and collects the body once, and `jsondec` takes the refinement path so a
/// `where` clause is decoded-then-guarded. Those are three answers to a
/// different question — how a name is *presented* — and folding them together
/// would be forcing the shared answer to express something it is not about.
#[derive(Debug, Clone, PartialEq)]
pub enum Wire {
    /// `Int64` — a JSON number in integer syntax.
    Int,
    /// A sized integer. Its width is part of the wire contract.
    IntN {
        bits: u8,
        signed: bool,
    },
    Float,
    Float32,
    Bool,
    Str,
    /// `Option<T>` — the payload, or absent/`null`. Carries `T`.
    Option(Type),
    /// A growable `Array<T>`.
    Array(Type),
    /// `Array<T, N>` — an ordinary JSON array on the way OUT, and encode-only:
    /// its length is not known until the data arrives, so it is not a decode
    /// target.
    FixedArray(Type, usize),
    /// `Map<String, V>` — a JSON object. Carries `V`; the key is always `String`.
    Map(Type),
    Record(Vec<Field>),
    /// A sum type in RFC-0024 external tagging. `Result<T, E>` arrives here too,
    /// as the two-variant enum it is on the wire.
    Enum(Vec<EnumVariant>),
}

/// The wire form of `ty`, or `Err` with the resolved type that has none.
///
/// `decode` picks the direction: the decode domain is the narrower one (a fixed
/// array and a `lazy` field are encode-only), which is the only thing the two
/// directions disagree about.
pub fn wire(ty: &Type, types: &HashMap<String, TypeDecl>, decode: bool) -> Result<Wire, Type> {
    // `Validation<T>` stays off the wire in v1 (its `Invalid` carries an
    // `Array<Issue>` and its `Valid` a generic payload) — rejected by NAME,
    // before resolution, so the diagnostic says `Validation` rather than naming
    // the enum it resolves to (RFC-0024 out-of-scope).
    if is_validation(ty) {
        return Err(ty.clone());
    }
    // A `lazy T` field ENCODES as a `T`: `toJson` reads the field, the read
    // forces the thunk, and the JSON carries the value. JSON that silently
    // dropped a declared field would be worse than one that paid to compute it —
    // and it is precisely why RFC-0085 M4b needs a selection-aware encoder.
    //
    // It does NOT decode. A decoded value arrives as data with no thunk behind
    // it, so there is nothing to defer; a decoder that manufactured a constant
    // thunk would be laziness that had already done the work.
    if let Type::Lazy(inner) = ty {
        return if decode {
            Err(ty.clone())
        } else {
            wire(inner, types, decode)
        };
    }
    // A nested `Option` is a decode hazard (a double `null` has two readings), so
    // it has no wire form in either direction. An `Option<Result<..>>` /
    // `Option<Enum>` DOES (RFC-0024): a payload enum never encodes as `null`, so
    // the wire form stays unambiguous.
    if let Type::Option(inner) = ty {
        if matches!(crate::types::resolve(inner, types), Type::Option(_)) {
            return Err(ty.clone());
        }
    }
    let r = crate::types::resolve(ty, types);
    match r {
        Type::Int => Ok(Wire::Int),
        Type::IntN { bits, signed } => Ok(Wire::IntN { bits, signed }),
        Type::Float => Ok(Wire::Float),
        Type::Float32 => Ok(Wire::Float32),
        Type::Bool => Ok(Wire::Bool),
        Type::Str => Ok(Wire::Str),
        Type::Option(inner) => Ok(Wire::Option(*inner)),
        Type::Array(inner) => Ok(Wire::Array(*inner)),
        Type::ArrayN(inner, n) => {
            if decode {
                Err(Type::ArrayN(inner, n))
            } else {
                Ok(Wire::FixedArray(*inner, n))
            }
        }
        // The key is always `String` (the checker enforces it), so only the value
        // type is carried.
        Type::Map(_, val) => Ok(Wire::Map(*val)),
        Type::Record(fields) => Ok(Wire::Record(fields)),
        Type::Enum(vs) => Ok(Wire::Enum(vs)),
        // `Result<T, E>` flows through as the two-variant single-payload enum
        // `{"Ok":<T>}` / `{"Err":<E>}` (RFC-0024) — so it IS that enum here, and
        // every consumer gets the tagging rule once instead of four times.
        Type::Result(t, e) => Ok(Wire::Enum(vec![
            EnumVariant {
                name: "Ok".to_string(),
                payload: vec![*t],
            },
            EnumVariant {
                name: "Err".to_string(),
                payload: vec![*e],
            },
        ])),
        // Everything else is off the wire in v1: the vectors, `Ref`, `Task`,
        // `Stream`, `SmallArray`, `Template`, `Logger`, `Fn`, `Param`, `Unit`,
        // `ConstInt`, `Never`, `Err` — and any name that did not resolve.
        other => Err(other),
    }
}

/// Whether the resolved wire form of `ty` came from resolving a NAME — the six
/// heads that can be cyclic, and so the ones a walk over [`Wire`] has to guard
/// before it re-enters them.
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

/// The gate, as a walk over [`Wire`]: this decides nothing about what a type
/// becomes on the wire, it only asks whether every leaf reached that way has one.
///
/// What stays here rather than moving into [`wire`] is the DIAGNOSTIC: a
/// rejection is described by the spelling the user wrote, so a named type
/// re-badges its structural rejection with its own name and an enum payload
/// names its variant.
fn codable(
    ty: &Type,
    types: &HashMap<String, TypeDecl>,
    decode: bool,
    seen: &mut Vec<String>,
) -> Result<(), String> {
    // A named type is described by the *spelling the user wrote* in error
    // messages, but we recurse through its structural form.
    let display = type_display(ty);
    if is_validation(ty) {
        return Err("Validation".to_string());
    }
    // A type whose wire form comes from resolving a NAME can be self-referential
    // (`type Node = { kids: Array<Node> }`), so the walk guards on the spelling
    // it will re-enter. `Named` additionally re-badges, which is why it is the
    // one head spelled out.
    if let Type::Named(n) = ty {
        if seen.iter().any(|s| s == n) {
            return Ok(()); // already being checked — break the cycle
        }
        let Some(d) = types.get(n) else {
            return Err(n.clone());
        };
        seen.push(n.clone());
        let r = codable_wire(ty, &display, types, decode, seen);
        seen.pop();
        // Preserve a payload-enum's precise variant/payload offender; re-badge
        // any other structural rejection with the user's name.
        return r.map_err(|e| {
            if matches!(&d.base, Type::Enum(vs) if vs.iter().any(|v| !v.payload.is_empty()))
                || matches!(d.base, Type::Result(..))
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

/// `codable` once the cycle guard and the naming are out of the way: ask [`wire`]
/// what the type is, and recurse on what it carries. `display` is the user's
/// spelling of `ty`, which this level cannot re-derive after resolution.
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
            | Wire::Map(inner),
        ) => codable(&inner, types, decode, seen),
        Ok(Wire::Record(fields)) => {
            for f in &fields {
                codable(&f.ty, types, decode, seen)?;
            }
            Ok(())
        }
        // A payload enum is codable when every variant's payloads are (RFC-0024).
        // A rejection names the offending variant + payload type so the diagnostic
        // is precise (`Task<Int64> (payload of variant \`Boxed\`)`).
        Ok(Wire::Enum(vs)) => enum_codable(&vs, types, decode, seen),
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

    /// One inhabitant of every variant of the type enum, held complete by
    /// [`Type::VARIANTS`] and [`Type::variant_name`] — the same lock PR #173
    /// put on `layout::SHAPES`, asked here about the wire form.
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
            Type::Option(b(Type::Int)),
            Type::Result(b(Type::Int), b(Type::Str)),
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
            Type::Task(b(Type::Int)),
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

    /// The guard the four `match`es could not be for each other.
    ///
    /// "Which types cross the JSON wire, and as what" used to be answered
    /// independently by [`codable`], `types::type_schema`, `jsonenc::Walk::body`
    /// and `jsondec::Walk::body` — three of them with an open `_` arm, so a new
    /// variant could be admitted by one and unknown to the others with no
    /// compile-time complaint. It had already happened: the two synthesizers
    /// resolved first and the two others did not, so every record transformer and
    /// every applied generic got three answers.
    ///
    /// [`wire`] is the one answer now, and this holds it complete the way PR
    /// #173 holds `layout::SHAPES`: the cases are DERIVED from the type enum
    /// rather than typed out. [`Type::variant_name`]'s match is exhaustive, so a
    /// new variant stops this file compiling; [`Type::VARIANTS`] is the same set
    /// as data, so a variant that gained an arm but no seed still fails.
    ///
    /// What it asserts is not that each verdict is a particular one — that is
    /// what the rows below are for — but that there IS a verdict, reached
    /// without panicking, for every variant and for a few thousand trees built
    /// out of them, in both directions.
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

        // Every container over every seed, twice, plus every ordered pair
        // through the two-argument constructors.
        let mut all = seeds.clone();
        for t in &seeds {
            let b = || Box::new(t.clone());
            all.extend([
                Type::Option(b()),
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
                all.push(Type::Result(Box::new(a.clone()), Box::new(c.clone())));
                all.push(Type::Option(Box::new(Type::Result(
                    Box::new(a.clone()),
                    Box::new(c.clone()),
                ))));
            }
        }
        assert!(all.len() > 500, "coverage shrank to {}", all.len());

        for ty in &all {
            for decode in [false, true] {
                // A verdict, either way — the point is that no variant falls
                // through anything.
                let _ = wire(ty, &types, decode);
                // And the gate agrees about REACHABILITY: it rejects exactly
                // when the shared answer has no wire form for the leaf it walks
                // to, so the checker can never refuse what the synthesizers
                // would have written (which is the drift this replaced).
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

    /// The rows the four sites used to disagree about, pinned.
    ///
    /// A record transformer IS the record it computes (RFC-0002 §7) and an
    /// applied generic IS its substituted base, so both cross the wire. Before
    /// [`wire`] the checker's gate refused them, the schema emitter described
    /// them as `{}` ("anything"), and the encoder and decoder wrote them out in
    /// full — three answers, and the gate's was the one users met.
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

    /// A nested `Option` has no wire form however it is SPELLED.
    ///
    /// The rejection used to be `matches!(**inner, Type::Option(_))` on the RAW
    /// inner, so one type alias walked straight past it: on `main`,
    /// `type MaybeInt = Option<Int64>` made `toJson(x: Option<MaybeInt>)`
    /// compile, and `Some(None)` and `None` both wrote `null` — a value nothing
    /// can read back. The same defect as the transformers, with the sign
    /// reversed: a decision about a wire SHAPE was taken on a SPELLING.
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
                base: Type::Option(Box::new(Type::Int)),
                predicate: None,
                line: 0,
            },
        );
        let aliased = Type::Option(Box::new(Type::Named("MaybeInt".into())));
        let bare = Type::Option(Box::new(Type::Option(Box::new(Type::Int))));
        for ty in [&aliased, &bare] {
            assert!(encodable(ty, &types).is_err(), "{ty} encodes");
            assert!(decodable(ty, &types).is_err(), "{ty} decodes");
            assert!(wire(ty, &types, false).is_err(), "{ty} has a wire form");
        }
        // A single `Option` through the same alias is untouched.
        assert!(encodable(&Type::Named("MaybeInt".into()), &types).is_ok());
    }

    /// A self-referential type terminates through every head that resolves, not
    /// just through a name. `wire` resolves, so the gate's cycle guard has to
    /// cover the transformer heads too — without it, `type L = { next: L }`
    /// reached through `Partial<L>` walks forever.
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

        // A non-codable value type (a `Task`) makes the whole map non-codable,
        // and the offender is named.
        let bad = Type::Map(
            Box::new(Type::Str),
            Box::new(Type::Task(Box::new(Type::Int))),
        );
        assert_eq!(encodable(&bad, &types).unwrap_err(), "Task");
    }

    /// The decode-side `expected` phrase for a map value is `object` (a Map IS a
    /// JSON object) — the wording that lands in a `json.type` Issue.
    #[test]
    fn map_expected_name_is_object() {
        let types = HashMap::new();
        let m = Type::Map(Box::new(Type::Str), Box::new(Type::Int));
        assert_eq!(expected_name(&m, &types), "object");
    }

    // ---- the exact-integer parser ------------------------------------------

    /// A paired `\uD83D\uDE00` escape IS 😀 — how RFC 8259 §7 spells an astral
    /// codepoint and what `std/jsonread` decodes — so the parity surface
    /// decodes it too, and re-escaping round-trips. A lone or unpaired half is
    /// refused with the same wording as any other bad character.
    #[test]
    fn a_surrogate_pair_escape_decodes_and_round_trips() {
        let face = JsonV::Str("😀".to_string());
        assert_eq!(parse(r#""😀""#).unwrap(), face);
        assert_eq!(parse(r#""\ud83d\ude00""#).unwrap(), face);
        assert_eq!(parse(r#""\uD83D\uDE00""#).unwrap(), face);
        // Round-trip: the canonical escaper writes the scalar verbatim, and
        // the pair spelling reads back to the same string.
        let mut body = String::new();
        escape_into(&"😀".to_string(), &mut body);
        assert_eq!(parse(&format!("\"{body}\"")).unwrap(), face);

        for bad in [
            r#""\ud83d""#,       // high half, nothing follows
            r#""\ud83dx""#,      // high half, not an escape
            r#""\ud83d\n""#,     // high half, a different escape
            r#""\ud83d\u0041""#, // high half, not a low half
            r#""\ude00""#,       // lone low half
            r#""\udbff""#,       // lone high half at the top of the range
        ] {
            assert!(parse(bad).is_err(), "{bad} parses");
        }
    }

    /// The parser recurses once per enclosing `{`/`[`, so an unbounded document
    /// was a stack overflow — an abort, no diagnostic. The bound is the one
    /// [`crate::schema`] takes, read from that constant so the two parsers
    /// cannot drift.
    #[test]
    fn a_document_deeper_than_the_limit_is_refused_not_crashed() {
        let nest = |d: usize| format!("{}{}", "[".repeat(d), "]".repeat(d));
        assert!(
            parse(&nest(crate::schema::MAX_JSON_DEPTH)).is_ok(),
            "at the limit is inside it"
        );
        let e = parse(&nest(crate::schema::MAX_JSON_DEPTH + 1)).unwrap_err();
        assert!(e.0.contains("deeper"), "{e:?}");
        // Depth is not length: a hundred thousand siblings are fine.
        assert!(parse(&nest(100_000)).is_err());
        let wide = format!("[{}]", vec!["1"; 100_000].join(","));
        assert!(parse(&wide).is_ok());
    }

    /// The integer grammar is `0 | [1-9][0-9]*` and a name is defined once:
    /// the two leniencies every strict reader (`std/jsonread`, `crate::schema`)
    /// refuses, refused here too so this parser stays in lockstep with the
    /// reader it documents.
    #[test]
    fn leading_zeros_and_duplicate_keys_are_refused() {
        for bad in ["01", "-01", "{\"a\":1,\"a\":2}"] {
            assert!(parse(bad).is_err(), "{bad} parses");
        }
        let e = parse("{\"a\":1,\"a\":2}").unwrap_err();
        assert!(e.0.contains("`a`"), "names the key: {e:?}");
        for good in ["0", "-0", "10", "0.5", "{\"a\":1,\"b\":2}"] {
            assert!(parse(good).is_ok(), "{good} refused");
        }
    }
}

/// A user-facing spelling for a type, for the codability rejection message.
fn type_display(ty: &Type) -> String {
    match ty {
        Type::Named(n) => n.clone(),
        Type::Result(..) => "Result".to_string(),
        Type::Task(_) => "Task".to_string(),
        Type::Logger => "Logger".to_string(),
        Type::ArrayN(inner, n) => format!("Array<{}, {}>", type_display(inner), n),
        Type::Option(inner) => format!("Option<{}>", type_display(inner)),
        Type::Unit => "Unit".to_string(),
        other => format!("{other}"),
    }
}
