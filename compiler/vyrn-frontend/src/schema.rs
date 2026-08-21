//! JSON Schema type imports (RFC-0010 M2).
//!
//! `import type { User } from "./api.schema.json"` synthesizes Vyrn
//! [`TypeDecl`]s from a JSON Schema document — the exact **inverse** of the
//! `jsonSchema(T)` emitter in [`crate::types`]: `minimum`/`maximum`/
//! `exclusive*`/`multipleOf`/`not{const}` become `where` clauses over `value`,
//! `minLength`/`maxLength`/`pattern` become string refinements, `object` +
//! `required` becomes a record with `Option<T>` for optional fields, and
//! nested inline objects become synthetic named types (`User.address`),
//! reusing the inline-refinement machinery.
//!
//! **Anything the mapping cannot express is a hard error** naming the type
//! and keyword — an imported type is never silently weaker than its schema.
//! (`$comment`, `title`, `description`, `$schema`, and `examples`/`default`
//! are informational in JSON Schema and are ignored.)
//!
//! The JSON parser below is deliberately minimal (~150 lines, no crates):
//! objects (order-preserving), arrays, strings with escapes, numbers, bools,
//! null.

use crate::ast::*;

// ---------------------------------------------------------------------------
// minimal JSON
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    /// Insertion-ordered (field order matters for record layout).
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// How many `{`/`[` may enclose a value before the parser refuses the document.
///
/// This parser is recursive descent, so a document's nesting depth is the
/// process's stack depth, and it is reached from `find_manifest` — which walks
/// **up** from the working directory on every command and reads a `vyrn.json`
/// the user may not own. Without a bound, a manifest anywhere above the cwd ends
/// every `vyrn` invocation in a stack overflow: an abort, no diagnostic, exit
/// 127.
///
/// The number is the one `std/jsonread` and `std/von` took: about two frames per
/// enclosing level against the smallest stack any build profile gets, leaving
/// the rest for the caller and for the synthesis pass that runs over the parsed
/// document. No schema or manifest anyone writes by hand comes near it.
pub const MAX_JSON_DEPTH: usize = 128;

pub fn parse_json(src: &str) -> Result<Json, String> {
    let bytes: Vec<char> = src.chars().collect();
    let mut p = P {
        b: &bytes,
        i: 0,
        depth: 0,
    };
    p.ws();
    let v = p.value()?;
    p.ws();
    if p.i != p.b.len() {
        return Err(format!("trailing content at offset {}", p.i));
    }
    Ok(v)
}

struct P<'a> {
    b: &'a [char],
    i: usize,
    /// Objects and arrays currently open — the recursion this parser bounds.
    depth: usize,
}

impl P<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && self.b[self.i].is_whitespace() {
            self.i += 1;
        }
    }
    fn peek(&self) -> Option<char> {
        self.b.get(self.i).copied()
    }
    fn expect(&mut self, c: char) -> Result<(), String> {
        if self.peek() == Some(c) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("expected `{c}` at offset {}", self.i))
        }
    }
    /// Enter one enclosing level, or refuse. Every recursive call goes through
    /// here, so the bound is the parser's and not one caller's.
    fn nest(&mut self) -> Result<(), String> {
        self.depth += 1;
        if self.depth > MAX_JSON_DEPTH {
            return Err(format!(
                "nested deeper than {MAX_JSON_DEPTH} levels at offset {}",
                self.i
            ));
        }
        Ok(())
    }
    fn value(&mut self) -> Result<Json, String> {
        match self.peek() {
            Some('{') => {
                self.nest()?;
                let v = self.obj();
                self.depth -= 1;
                v
            }
            Some('[') => {
                self.nest()?;
                let v = self.arr();
                self.depth -= 1;
                v
            }
            Some('"') => Ok(Json::Str(self.string()?)),
            Some('t') => self.lit("true", Json::Bool(true)),
            Some('f') => self.lit("false", Json::Bool(false)),
            Some('n') => self.lit("null", Json::Null),
            Some(c) if c == '-' || c.is_ascii_digit() => self.num(),
            other => Err(format!("unexpected {other:?} at offset {}", self.i)),
        }
    }
    fn lit(&mut self, word: &str, v: Json) -> Result<Json, String> {
        for c in word.chars() {
            self.expect(c)?;
        }
        Ok(v)
    }
    fn obj(&mut self) -> Result<Json, String> {
        self.expect('{')?;
        let mut out = Vec::new();
        self.ws();
        if self.peek() == Some('}') {
            self.i += 1;
            return Ok(Json::Obj(out));
        }
        loop {
            self.ws();
            let k = self.string()?;
            self.ws();
            self.expect(':')?;
            self.ws();
            let v = self.value()?;
            // A name defined twice is a document with two meanings, and this
            // one has two readers: `Json::get` answers with the FIRST pair, and
            // the `$defs` walk below keeps the LAST. Whichever a caller asks,
            // the other one is wrong. Refused where `std/von` and `std/jsonread`
            // refuse it.
            if out.iter().any(|(prev, _)| prev == &k) {
                return Err(format!("`{k}` is defined twice at offset {}", self.i));
            }
            out.push((k, v));
            self.ws();
            match self.peek() {
                Some(',') => self.i += 1,
                Some('}') => {
                    self.i += 1;
                    return Ok(Json::Obj(out));
                }
                other => return Err(format!("expected `,` or `}}`, found {other:?}")),
            }
        }
    }
    fn arr(&mut self) -> Result<Json, String> {
        self.expect('[')?;
        let mut out = Vec::new();
        self.ws();
        if self.peek() == Some(']') {
            self.i += 1;
            return Ok(Json::Arr(out));
        }
        loop {
            self.ws();
            out.push(self.value()?);
            self.ws();
            match self.peek() {
                Some(',') => self.i += 1,
                Some(']') => {
                    self.i += 1;
                    return Ok(Json::Arr(out));
                }
                other => return Err(format!("expected `,` or `]`, found {other:?}")),
            }
        }
    }
    fn string(&mut self) -> Result<String, String> {
        self.expect('"')?;
        let mut out = String::new();
        loop {
            match self.peek() {
                None => return Err("unterminated string".into()),
                Some('"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some('\\') => {
                    self.i += 1;
                    let esc = self.peek().ok_or("unterminated escape")?;
                    self.i += 1;
                    match esc {
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        '/' => out.push('/'),
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        'r' => out.push('\r'),
                        'b' => out.push('\u{8}'),
                        'f' => out.push('\u{c}'),
                        'u' => {
                            let mut hex = String::new();
                            for _ in 0..4 {
                                hex.push(self.peek().ok_or("bad \\u escape")?);
                                self.i += 1;
                            }
                            let cp = u32::from_str_radix(&hex, 16)
                                .map_err(|_| "bad \\u escape".to_string())?;
                            // Surrogate pairs are rare in schemas; reject
                            // rather than mis-decode.
                            let ch = char::from_u32(cp)
                                .ok_or("surrogate \\u escapes are not supported")?;
                            out.push(ch);
                        }
                        other => return Err(format!("unknown escape \\{other}")),
                    }
                }
                Some(c) => {
                    // A raw control byte (< 0x20) is invalid in a JSON string
                    // (RFC 8259 §7) — the same rule codec.rs enforces on its
                    // own parser.
                    if c < '\u{20}' {
                        return Err(format!("control character in string at offset {}", self.i));
                    }
                    out.push(c);
                    self.i += 1;
                }
            }
        }
    }
    fn num(&mut self) -> Result<Json, String> {
        let start = self.i;
        if self.peek() == Some('-') {
            self.i += 1;
        }
        while self.peek().is_some_and(|c| {
            c.is_ascii_digit() || c == '.' || c == 'e' || c == 'E' || c == '+' || c == '-'
        }) {
            self.i += 1;
        }
        let text: String = self.b[start..self.i].iter().collect();
        text.parse::<f64>()
            .map(Json::Num)
            .map_err(|_| format!("bad number `{text}`"))
    }
}

// ---------------------------------------------------------------------------
// schema -> TypeDecl synthesis
// ---------------------------------------------------------------------------

/// Synthesize the requested types (plus everything they reference through
/// `$defs` and nested objects) from a JSON Schema document. `module` is the
/// resolved path (for decl attribution). Requested names are exported; the
/// helpers they pull in are not.
pub fn synthesize(
    source: &str,
    requested: Option<&[String]>,
    module: &str,
) -> Result<Vec<TypeDecl>, String> {
    let doc = parse_json(source).map_err(|e| format!("{module}: invalid JSON: {e}"))?;
    let mut out: Vec<TypeDecl> = Vec::new();

    // A name is found as the document root (matching `title`) or a $defs key.
    let defs = doc.get("$defs");
    let root_title = doc.get("title").and_then(|t| t.as_str());

    let mut pending: Vec<(String, &Json, bool)> = Vec::new(); // (name, schema, exported)
    match requested {
        Some(names) => {
            for name in names {
                if Some(name.as_str()) == root_title {
                    pending.push((name.clone(), &doc, true));
                } else if let Some(schema) = defs.and_then(|d| d.get(name)) {
                    pending.push((name.clone(), schema, true));
                } else {
                    return Err(format!(
                        "{module}: schema defines no type `{name}` (looked at the root \
                         `title` and `$defs`)"
                    ));
                }
            }
        }
        // The loader synthesizes everything the document defines; the ordinary
        // import machinery then filters by the names actually imported.
        None => {
            if let Some(title) = root_title {
                pending.push((title.to_string(), &doc, true));
            }
            if let Some(Json::Obj(entries)) = defs {
                for (name, schema) in entries {
                    pending.push((name.clone(), schema, true));
                }
            }
            if pending.is_empty() {
                return Err(format!(
                    "{module}: schema defines no importable types (no root `title`, \
                     no `$defs`)"
                ));
            }
        }
    }

    let mut done: std::collections::HashSet<String> = Default::default();
    while let Some((name, schema, exported)) = pending.pop() {
        if !done.insert(name.clone()) {
            continue;
        }
        let mut nested: Vec<TypeDecl> = Vec::new();
        let (base, predicate, mut extra) = convert(schema, &name, module, root_title, &mut nested)?;
        // $defs referenced via $ref come back in `extra`; queue unemitted ones.
        // A `$ref: "#"` back-edge references the root itself, which lives at
        // the document top, not under `$defs`.
        for r in extra.drain(..) {
            if !done.contains(&r) {
                let schema = if Some(r.as_str()) == root_title {
                    &doc
                } else {
                    defs.and_then(|d| d.get(&r)).ok_or_else(|| {
                        format!("{module}: `$ref` points at missing `#/$defs/{r}`")
                    })?
                };
                pending.push((r, schema, false));
            }
        }
        out.push(TypeDecl {
            name,
            exported,
            module: Some(module.to_string()),
            doc: doc_of(schema),
            type_params: Vec::new(),
            base,
            predicate,
            line: 1, // line 0 means "parser-injected" and would be deduped away
        });
        // Constrained inline fields/elements synthesized `Parent.field`
        // helper types (the mirror of inline `where` refinements).
        for mut d in nested {
            if done.insert(d.name.clone()) {
                d.module = Some(module.to_string());
                out.push(d);
            }
        }
    }
    Ok(out)
}

fn doc_of(schema: &Json) -> Option<String> {
    schema
        .get("description")
        .and_then(|d| d.as_str())
        .map(|s| s.to_string())
}

/// Known-informational keywords, ignored everywhere.
const INFORMATIONAL: &[&str] = &[
    "$schema",
    "$id",
    "title",
    "description",
    "$comment",
    "examples",
    "default",
    "$defs",
];

/// Convert one schema object into (base type, predicate, $defs-referenced
/// names to synthesize). Nested inline objects error (kept simple: name your
/// nested shapes via `$defs` — mirrors what our own emitter produces).
fn convert(
    schema: &Json,
    name: &str,
    module: &str,
    root: Option<&str>,
    nested: &mut Vec<TypeDecl>,
) -> Result<(Type, Option<Expr>, Vec<String>), String> {
    let Json::Obj(fields) = schema else {
        return Err(format!("{module}: type `{name}`: schema must be an object"));
    };

    // $ref-only schema.
    if let Some(r) = schema.get("$ref").and_then(|r| r.as_str()) {
        // `#` is the document root — a recursive back-edge the emitter
        // produces for self-referential types (`next: Option<Node>`).
        if r == "#" {
            let target = root.ok_or_else(|| {
                format!("{module}: type `{name}`: `$ref` `#` needs a root `title`")
            })?;
            return Ok((
                Type::Named(target.to_string()),
                None,
                vec![target.to_string()],
            ));
        }
        let target = r.strip_prefix("#/$defs/").ok_or_else(|| {
            format!(
                "{module}: type `{name}`: unsupported `$ref` `{r}` (only `#` and \
                 `#/$defs/..` are supported)"
            )
        })?;
        return Ok((
            Type::Named(target.to_string()),
            None,
            vec![target.to_string()],
        ));
    }

    // `enum` of strings → a payload-less Vyrn enum (each entry a nullary
    // variant) — the inverse of the emitter's sum-type encoding.
    if let Some(Json::Arr(items)) = schema.get("enum") {
        for (k, _) in fields {
            if k != "enum" && !INFORMATIONAL.contains(&k.as_str()) {
                return Err(format!(
                    "{module}: type `{name}`: unsupported keyword `{k}` alongside `enum` \
                     (Vyrn imports schemas exactly or not at all)"
                ));
            }
        }
        let mut variants = Vec::new();
        for it in items {
            match it {
                Json::Str(s) => variants.push(crate::ast::EnumVariant {
                    name: s.clone(),
                    payload: Vec::new(),
                }),
                _ => {
                    return Err(format!(
                        "{module}: type `{name}`: `enum` entries must all be strings"
                    ))
                }
            }
        }
        return Ok((Type::Enum(variants), None, Vec::new()));
    }

    // `oneOf` of RFC-0024 externally-tagged members → a payload Vyrn enum (the
    // inverse of the emitter's `oneOf`). Exactly-or-not-at-all: any member that
    // is not a `{"const":"Name"}` nullary or a single-property tagged object is a
    // hard error.
    if let Some(Json::Arr(members)) = schema.get("oneOf") {
        for (k, _) in fields {
            if k != "oneOf" && !INFORMATIONAL.contains(&k.as_str()) {
                return Err(format!(
                    "{module}: type `{name}`: unsupported keyword `{k}` alongside `oneOf` \
                     (Vyrn imports schemas exactly or not at all)"
                ));
            }
        }
        let mut variants = Vec::new();
        let mut refs = Vec::new();
        for m in members {
            variants.push(convert_oneof_member(
                m, name, module, root, nested, &mut refs,
            )?);
        }
        return Ok((Type::Enum(variants), None, refs));
    }

    let ty = schema
        .get("type")
        .and_then(|t| t.as_str())
        .ok_or_else(|| format!("{module}: type `{name}`: schema has no `type` (or `$ref`)"))?;

    let allowed: &[&str] = match ty {
        "integer" | "number" => &[
            "type",
            "minimum",
            "maximum",
            "exclusiveMinimum",
            "exclusiveMaximum",
            "multipleOf",
            "not",
        ],
        "string" => &["type", "minLength", "maxLength", "pattern", "allOf"],
        "boolean" => &["type"],
        "object" => &["type", "properties", "required", "additionalProperties"],
        "array" => &["type", "items"],
        other => {
            return Err(format!(
                "{module}: type `{name}`: unsupported JSON Schema `type` `{other}`"
            ))
        }
    };
    for (k, _) in fields {
        if !allowed.contains(&k.as_str()) && !INFORMATIONAL.contains(&k.as_str()) {
            return Err(format!(
                "{module}: type `{name}`: unsupported JSON Schema keyword `{k}` \
                 (Vyrn imports schemas exactly or not at all)"
            ));
        }
    }

    let mut refs = Vec::new();
    match ty {
        "integer" | "number" => {
            let is_int = ty == "integer";
            let base = if is_int { Type::Int } else { Type::Float };
            let mut clauses: Vec<Expr> = Vec::new();
            let bound = |key: &str, op: BinOp, clauses: &mut Vec<Expr>| -> Result<(), String> {
                if let Some(Json::Num(n)) = schema.get(key) {
                    clauses.push(cmp(op, num_expr(*n, is_int, module, name, key)?));
                }
                Ok(())
            };
            bound("minimum", BinOp::GtEq, &mut clauses)?;
            bound("exclusiveMinimum", BinOp::Gt, &mut clauses)?;
            bound("maximum", BinOp::LtEq, &mut clauses)?;
            bound("exclusiveMaximum", BinOp::Lt, &mut clauses)?;
            if let Some(Json::Num(k)) = schema.get("multipleOf") {
                let k = num_expr(*k, is_int, module, name, "multipleOf")?;
                clauses.push(Expr::Binary {
                    op: BinOp::Eq,
                    lhs: Box::new(Expr::Binary {
                        op: BinOp::Rem,
                        lhs: Box::new(value_var()),
                        rhs: Box::new(k),
                        line: 1,
                    }),
                    rhs: Box::new(Expr::Int(0)),
                    line: 1,
                });
            }
            if let Some(not) = schema.get("not") {
                let c = not.get("const").ok_or_else(|| {
                    format!("{module}: type `{name}`: unsupported `not` (only `not.const`)")
                })?;
                let Json::Num(n) = c else {
                    return Err(format!(
                        "{module}: type `{name}`: `not.const` must be a number here"
                    ));
                };
                clauses.push(cmp(
                    BinOp::NotEq,
                    num_expr(*n, is_int, module, name, "not.const")?,
                ));
            }
            Ok((base, conjoin(clauses), refs))
        }
        "boolean" => Ok((Type::Bool, None, refs)),
        "string" => {
            let mut clauses: Vec<Expr> = Vec::new();
            if let Some(Json::Num(n)) = schema.get("minLength") {
                // Lengths are whole numbers, so a fractional minimum ceils
                // (`2.5` admits length ≥ 3, and truncating toward zero would
                // import a type weaker than its schema); a negative one cannot
                // be honored at all — hard error, never a vacuous clause.
                if *n < 0.0 {
                    return Err(format!(
                        "{module}: type `{name}`: `minLength` {n} is negative"
                    ));
                }
                clauses.push(len_cmp(BinOp::GtEq, n.ceil() as i64));
            }
            if let Some(Json::Num(n)) = schema.get("maxLength") {
                clauses.push(len_cmp(BinOp::LtEq, *n as i64));
            }
            let mut add_pattern = |p: &str| -> Result<(), String> {
                let inner = p.strip_prefix('^').unwrap_or(p);
                let inner = inner.strip_suffix('$').unwrap_or(inner);
                crate::regex::compile(inner).map_err(|e| {
                    format!(
                        "{module}: type `{name}`: `pattern` `{p}` is outside Vyrn's \
                         regex subset: {e}"
                    )
                })?;
                clauses.push(Expr::Binary {
                    op: BinOp::Match,
                    lhs: Box::new(value_var()),
                    rhs: Box::new(Expr::Str(inner.to_string())),
                    line: 1,
                });
                Ok(())
            };
            if let Some(Json::Str(p)) = schema.get("pattern") {
                add_pattern(p)?;
            }
            if let Some(Json::Arr(parts)) = schema.get("allOf") {
                for part in parts {
                    match part.get("pattern").and_then(|p| p.as_str()) {
                        Some(p) => add_pattern(p)?,
                        None => {
                            return Err(format!(
                                "{module}: type `{name}`: unsupported `allOf` member \
                                 (only `{{\"pattern\": ..}}` entries)"
                            ));
                        }
                    }
                }
            }
            Ok((Type::Str, conjoin(clauses), refs))
        }
        "array" => {
            let items = schema
                .get("items")
                .ok_or_else(|| format!("{module}: type `{name}`: `array` schema needs `items`"))?;
            let (inner, pred, mut r) =
                convert(items, &format!("{name}.item"), module, root, nested)?;
            refs.append(&mut r);
            let inner = if pred.is_some() {
                // Constrained elements become a synthetic validated type, so
                // every element auto-validates at its boundaries.
                nested.push(TypeDecl {
                    name: format!("{name}.item"),
                    exported: false,
                    module: None,
                    doc: None,
                    type_params: Vec::new(),
                    base: inner,
                    predicate: pred,
                    line: 1,
                });
                Type::Named(format!("{name}.item"))
            } else {
                inner
            };
            Ok((Type::Array(Box::new(inner)), None, refs))
        }
        "object" => {
            // An `additionalProperties` object is a `Map<String, V>` (RFC-0028),
            // the inverse of the emitter. It carries no `properties`/`required`
            // (a mixed record+index-signature type is not expressible in Vyrn).
            if let Some(ap) = schema.get("additionalProperties") {
                if schema.get("properties").is_some() || schema.get("required").is_some() {
                    return Err(format!(
                        "{module}: type `{name}`: an `additionalProperties` object (a Map) \
                         cannot also carry `properties`/`required`"
                    ));
                }
                let (inner, pred, mut r) =
                    convert(ap, &format!("{name}.value"), module, root, nested)?;
                refs.append(&mut r);
                let inner = if pred.is_some() {
                    // Constrained values become a synthetic validated type so
                    // each value auto-validates at its boundaries (mirrors the
                    // array-`items` handling above).
                    nested.push(TypeDecl {
                        name: format!("{name}.value"),
                        exported: false,
                        module: None,
                        doc: None,
                        type_params: Vec::new(),
                        base: inner,
                        predicate: pred,
                        line: 1,
                    });
                    Type::Named(format!("{name}.value"))
                } else {
                    inner
                };
                return Ok((Type::Map(Box::new(Type::Str), Box::new(inner)), None, refs));
            }
            let props = match schema.get("properties") {
                Some(Json::Obj(props)) => props.clone(),
                // Present but not an object is a typo, and reading it as "no
                // fields" would import a type weaker than its schema.
                Some(_) => {
                    return Err(format!(
                        "{module}: type `{name}`: `properties` must be an object"
                    ));
                }
                None => Vec::new(),
            };
            let required: Vec<&str> = match schema.get("required") {
                Some(Json::Arr(names)) => names.iter().filter_map(|n| n.as_str()).collect(),
                _ => Vec::new(),
            };
            // A `required` entry naming no property is a presence check this
            // record cannot carry (decode ignores unknown fields), so it would
            // silently weaken the schema into nothing.
            let dangling: Vec<&str> = required
                .iter()
                .copied()
                .filter(|r| !props.iter().any(|(k, _)| k == *r))
                .collect();
            if !dangling.is_empty() {
                return Err(format!(
                    "{module}: type `{name}`: `required` names `{}`, but `properties` \
                     does not define {}",
                    dangling.join("`, `"),
                    if dangling.len() == 1 { "it" } else { "them" }
                ));
            }
            let mut rec_fields = Vec::new();
            for (fname, fschema) in &props {
                let sub_name = format!("{name}.{fname}");
                let (fty, fpred, mut r) = convert(fschema, &sub_name, module, root, nested)?;
                refs.append(&mut r);
                // A constrained (or nested-object) field becomes a synthetic
                // named type — the mirror of inline `where` refinements, so
                // the emitter's inlined per-property constraints round-trip.
                let fty = if fpred.is_some() || matches!(fty, Type::Record(_)) {
                    nested.push(TypeDecl {
                        name: sub_name.clone(),
                        exported: false,
                        module: None,
                        doc: doc_of(fschema),
                        type_params: Vec::new(),
                        base: fty,
                        predicate: fpred,
                        line: 1,
                    });
                    Type::Named(sub_name)
                } else {
                    fty
                };
                let fty = if required.contains(&fname.as_str()) {
                    fty
                } else {
                    Type::Option(Box::new(fty))
                };
                rec_fields.push(Field {
                    name: fname.clone(),
                    ty: fty,
                });
            }
            Ok((Type::Record(rec_fields), None, refs))
        }
        _ => unreachable!("filtered above"),
    }
}

/// Convert one RFC-0024 `oneOf` member into an [`EnumVariant`]. A
/// `{"const":"Name"}` is a nullary variant; a `{"type":"object","properties":
/// {"Name":<sub>},"required":["Name"]}` is a payload variant (single payload
/// direct, tuple payload via `{"type":"array","prefixItems":[..],"items":false}`).
/// Anything else is a hard error (exactly-or-not-at-all).
fn convert_oneof_member(
    m: &Json,
    name: &str,
    module: &str,
    root: Option<&str>,
    nested: &mut Vec<TypeDecl>,
    refs: &mut Vec<String>,
) -> Result<crate::ast::EnumVariant, String> {
    let bad = || {
        format!(
            "{module}: type `{name}`: unsupported `oneOf` member (only \
             `{{\"const\":\"Name\"}}` or a single-property tagged object)"
        )
    };
    let Json::Obj(mfields) = m else {
        return Err(bad());
    };
    // Nullary: `{"const":"Name"}`.
    if let Some(Json::Str(cname)) = m.get("const") {
        for (k, _) in mfields {
            if k != "const" && !INFORMATIONAL.contains(&k.as_str()) {
                return Err(bad());
            }
        }
        return Ok(crate::ast::EnumVariant {
            name: cname.clone(),
            payload: Vec::new(),
        });
    }
    // Payload: a single-property tagged object.
    if m.get("type").and_then(|t| t.as_str()) == Some("object") {
        for (k, _) in mfields {
            if !["type", "properties", "required"].contains(&k.as_str())
                && !INFORMATIONAL.contains(&k.as_str())
            {
                return Err(bad());
            }
        }
        let props = match m.get("properties") {
            Some(Json::Obj(p)) if p.len() == 1 => p,
            _ => return Err(bad()),
        };
        let (vname, sub) = &props[0];
        match m.get("required") {
            Some(Json::Arr(r)) if r.len() == 1 && r[0].as_str() == Some(vname.as_str()) => {}
            _ => return Err(bad()),
        }
        // Tuple payload: `{"type":"array","prefixItems":[..],"items":false}`.
        if sub.get("type").and_then(|t| t.as_str()) == Some("array")
            && sub.get("prefixItems").is_some()
        {
            let Json::Obj(sfields) = sub else {
                return Err(bad());
            };
            for (k, _) in sfields {
                if !["type", "prefixItems", "items"].contains(&k.as_str())
                    && !INFORMATIONAL.contains(&k.as_str())
                {
                    return Err(bad());
                }
            }
            if sub.get("items") != Some(&Json::Bool(false)) {
                return Err(format!(
                    "{module}: type `{name}`: `oneOf` tuple payload needs `\"items\":false`"
                ));
            }
            let Some(Json::Arr(pis)) = sub.get("prefixItems") else {
                return Err(bad());
            };
            let mut payload = Vec::new();
            for (i, pi) in pis.iter().enumerate() {
                let sub_name = format!("{name}.{vname}.{i}");
                payload.push(convert_payload_type(
                    pi, &sub_name, module, root, nested, refs,
                )?);
            }
            return Ok(crate::ast::EnumVariant {
                name: vname.clone(),
                payload,
            });
        }
        // Single payload.
        let sub_name = format!("{name}.{vname}");
        let pty = convert_payload_type(sub, &sub_name, module, root, nested, refs)?;
        return Ok(crate::ast::EnumVariant {
            name: vname.clone(),
            payload: vec![pty],
        });
    }
    Err(bad())
}

/// Convert an enum-payload schema to a type, promoting a constrained or nested
/// shape to a synthetic named type (the mirror of the record-field / array-item
/// handling), so an inline refinement in a payload round-trips.
fn convert_payload_type(
    schema: &Json,
    sub_name: &str,
    module: &str,
    root: Option<&str>,
    nested: &mut Vec<TypeDecl>,
    refs: &mut Vec<String>,
) -> Result<Type, String> {
    let (pty, ppred, mut r) = convert(schema, sub_name, module, root, nested)?;
    refs.append(&mut r);
    if ppred.is_some() || matches!(pty, Type::Record(_)) {
        nested.push(TypeDecl {
            name: sub_name.to_string(),
            exported: false,
            module: None,
            doc: None,
            type_params: Vec::new(),
            base: pty,
            predicate: ppred,
            line: 1,
        });
        Ok(Type::Named(sub_name.to_string()))
    } else {
        Ok(pty)
    }
}

fn value_var() -> Expr {
    Expr::Var {
        name: "value".to_string(),
        line: 1,
    }
}

fn cmp(op: BinOp, rhs: Expr) -> Expr {
    Expr::Binary {
        op,
        lhs: Box::new(value_var()),
        rhs: Box::new(rhs),
        line: 1,
    }
}

fn len_cmp(op: BinOp, n: i64) -> Expr {
    Expr::Binary {
        op,
        lhs: Box::new(Expr::Field {
            expr: Box::new(value_var()),
            field: "byteLength".to_string(),
            line: 1,
        }),
        rhs: Box::new(Expr::Int(n)),
        line: 1,
    }
}

fn num_expr(n: f64, is_int: bool, module: &str, name: &str, key: &str) -> Result<Expr, String> {
    if is_int {
        if n.fract() != 0.0 {
            return Err(format!(
                "{module}: type `{name}`: `{key}` {n} is not an integer but the type is"
            ));
        }
        // Negate in Rust arithmetic, NOT through the AST: the old
        // `Expr::Int(-n as i64)` under a `Unary::Neg` had two defects. Rust
        // unary minus binds tighter than `as`, so `-n` saturated at
        // `i64::MAX` and a minimum of exactly `i64::MIN` came out one too
        // high; and the synthesized `Unary::Neg` hid the literal from
        // reflection (`predicate_bounds`). The one value whose negation
        // overflows IS `i64::MIN`, which is already correct for an inclusive
        // minimum — emitted as a plain negative literal.
        Ok(Expr::Int(if n < 0.0 {
            let a = -n;
            if a >= 9223372036854775808.0 {
                i64::MIN
            } else {
                -(a as i64)
            }
        } else {
            n as i64
        }))
    } else if n < 0.0 {
        Ok(Expr::Unary {
            op: UnOp::Neg,
            expr: Box::new(Expr::Float(-n)),
            line: 1,
        })
    } else {
        Ok(Expr::Float(n))
    }
}

fn conjoin(mut clauses: Vec<Expr>) -> Option<Expr> {
    let first = if clauses.is_empty() {
        return None;
    } else {
        clauses.remove(0)
    };
    Some(clauses.into_iter().fold(first, |acc, c| Expr::Binary {
        op: BinOp::And,
        lhs: Box::new(acc),
        rhs: Box::new(c),
        line: 1,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn json_parser_handles_the_usual_shapes() {
        let j = parse_json(r#"{"a": [1, -2.5, "s\n", true, null], "b": {"c": 3}}"#).unwrap();
        assert_eq!(j.get("b").and_then(|b| b.get("c")), Some(&Json::Num(3.0)));
        match j.get("a") {
            Some(Json::Arr(items)) => {
                assert_eq!(items[0], Json::Num(1.0));
                assert_eq!(items[1], Json::Num(-2.5));
                assert_eq!(items[2], Json::Str("s\n".into()));
                assert_eq!(items[3], Json::Bool(true));
                assert_eq!(items[4], Json::Null);
            }
            other => panic!("expected array, got {other:?}"),
        }
        assert!(parse_json("{\"a\": }").is_err());
        assert!(parse_json("[1, 2] trailing").is_err());
    }

    /// The parser recurses, so a document's nesting is the process's stack, and
    /// `find_manifest` reads a `vyrn.json` from every ancestor of the working
    /// directory on every command. Without a bound the answer to a deep one was a
    /// stack overflow: an abort, no diagnostic. The limit is read from the code
    /// that enforces it, so this cannot pin a number the parser no longer takes.
    #[test]
    fn a_document_deeper_than_the_limit_is_refused_not_crashed() {
        let nest = |d: usize| format!("{}{}", "[".repeat(d), "]".repeat(d));
        assert!(
            parse_json(&nest(MAX_JSON_DEPTH)).is_ok(),
            "at the limit is inside it"
        );
        let e = parse_json(&nest(MAX_JSON_DEPTH + 1)).unwrap_err();
        assert!(e.contains(&MAX_JSON_DEPTH.to_string()), "{e}");
        // Objects nest through the same door, and so does the mixture.
        let obj = format!(
            "{}1{}",
            "{\"a\":".repeat(MAX_JSON_DEPTH + 1),
            "}".repeat(MAX_JSON_DEPTH + 1)
        );
        assert!(parse_json(&obj).is_err());
        // Depth is not length: a million siblings at one level are fine.
        let wide = format!("[{}]", vec!["1"; 100_000].join(","));
        assert!(parse_json(&wide).is_ok());
    }

    /// One name, one meaning. The two readers of an object disagreed about a
    /// repeated key — `get` took the first pair and the `$defs` walk took the
    /// last — so the document said different things depending on who asked.
    #[test]
    fn a_name_defined_twice_is_refused() {
        let e = parse_json(r#"{"main":"a.vyrn","main":"b.vyrn"}"#).unwrap_err();
        assert!(e.contains("main"), "names the key: {e}");
        assert!(
            parse_json(r#"{"a":{"x":1},"b":{"x":2}}"#).is_ok(),
            "a repeat in a SIBLING object is a different name"
        );
    }

    fn synth(doc: &str) -> Vec<TypeDecl> {
        synthesize(doc, None, "t.json").unwrap()
    }

    #[test]
    fn integer_bounds_become_where_clauses() {
        let decls =
            synth(r#"{"title": "Port", "type": "integer", "minimum": 1, "maximum": 65535}"#);
        let port = &decls[0];
        assert_eq!(port.name, "Port");
        assert_eq!(port.base, Type::Int);
        let pred = crate::checker::pred_summary(port.predicate.as_ref().unwrap());
        assert_eq!(pred, "value >= 1 && value <= 65535");
    }

    #[test]
    fn string_constraints_and_pattern() {
        let decls =
            synth(r#"{"title": "Slug", "type": "string", "minLength": 1, "pattern": "^[a-z]+$"}"#);
        let pred = crate::checker::pred_summary(decls[0].predicate.as_ref().unwrap());
        assert_eq!(pred, "value.byteLength >= 1 && value =~ \"[a-z]+\"");
    }

    #[test]
    fn object_maps_to_record_with_optionals_and_synthetic_field_types() {
        let decls = synth(
            r#"{"title": "User", "type": "object",
                "properties": {"age": {"type": "integer", "minimum": 18},
                               "nick": {"type": "string"}},
                "required": ["age"]}"#,
        );
        let user = decls.iter().find(|d| d.name == "User").unwrap();
        let Type::Record(fields) = &user.base else {
            panic!("record")
        };
        assert_eq!(fields[0].ty, Type::Named("User.age".into()));
        assert_eq!(fields[1].ty, Type::Option(Box::new(Type::Str)));
        let age = decls.iter().find(|d| d.name == "User.age").unwrap();
        assert!(age.predicate.is_some());
        assert!(!age.exported, "synthetic helpers stay private");
    }

    #[test]
    fn refs_resolve_through_defs() {
        let decls = synth(
            r##"{"title": "Server", "type": "object",
                "properties": {"port": {"$ref": "#/$defs/Port"}},
                "required": ["port"],
                "$defs": {"Port": {"type": "integer", "minimum": 1}}}"##,
        );
        let server = decls.iter().find(|d| d.name == "Server").unwrap();
        let Type::Record(fields) = &server.base else {
            panic!("record")
        };
        assert_eq!(fields[0].ty, Type::Named("Port".into()));
        assert!(decls.iter().any(|d| d.name == "Port"));
    }

    #[test]
    fn unsupported_keywords_are_hard_errors() {
        for (doc, needle) in [
            (
                r#"{"title": "X", "type": "string", "format": "email"}"#,
                "`format`",
            ),
            (
                r#"{"title": "X", "oneOf": [{"type": "string"}]}"#,
                "unsupported `oneOf` member",
            ),
            (
                r#"{"title": "X", "type": "integer", "exclusiveMaximum": 5, "weird": 1}"#,
                "`weird`",
            ),
            (
                r#"{"title": "X", "type": "string", "pattern": "a(?=b)"}"#,
                "regex subset",
            ),
        ] {
            let e = synthesize(doc, None, "t.json").unwrap_err();
            assert!(e.contains(needle), "doc: {doc}\nerror: {e}");
        }
    }

    #[test]
    fn requesting_a_missing_name_errors() {
        let e = synthesize(
            r#"{"title": "A", "type": "boolean"}"#,
            Some(&["B".to_string()]),
            "t.json",
        )
        .unwrap_err();
        assert!(e.contains("no type `B`"), "{e}");
    }

    #[test]
    fn round_trips_with_the_jsonschema_emitter() {
        // Emit a schema from Vyrn types, import it back, re-emit: byte-equal.
        let src = "type Username = String where value.byteLength >= 3 && value.byteLength <= 16 \
                   type Age = Int64 where value >= 18 && value <= 130 \
                   type User = { name: Username, age: Age, nick: Option<String> } \
                   fn main() -> Int64 { return 0 }";
        let program = crate::check(src).unwrap();
        let types: HashMap<String, TypeDecl> = program
            .type_decls
            .iter()
            .map(|t| (t.name.clone(), t.clone()))
            .collect();
        let emitted = crate::types::json_schema_string(&types["User"], &types);
        // Give the document a title so the importer can bind the root.
        let doc = emitted.replacen("{", "{\"title\":\"User\",", 1);

        let decls = synthesize(&doc, Some(&["User".to_string()]), "t.json").unwrap();
        let reimported: HashMap<String, TypeDecl> =
            decls.iter().map(|t| (t.name.clone(), t.clone())).collect();
        let reemitted = crate::types::json_schema_string(&reimported["User"], &reimported);
        assert_eq!(emitted, reemitted, "schema round-trip must be exact");
    }

    /// A `Map<String, V>` field emits `additionalProperties`, and the importer
    /// round-trips exactly that shape back (RFC-0028): emit → import → re-emit
    /// byte-equal.
    #[test]
    fn map_additional_properties_round_trips() {
        let src = "type Counts = Map<String, Int64> \
                   type Bag = { counts: Counts } \
                   fn main() -> Int64 { return 0 }";
        let program = crate::check(src).unwrap();
        let types: HashMap<String, TypeDecl> = program
            .type_decls
            .iter()
            .map(|t| (t.name.clone(), t.clone()))
            .collect();
        let emitted = crate::types::json_schema_string(&types["Bag"], &types);
        assert!(
            emitted.contains("\"additionalProperties\":{\"type\":\"integer\"}"),
            "{emitted}"
        );
        let doc = emitted.replacen("{", "{\"title\":\"Bag\",", 1);
        let decls = synthesize(&doc, Some(&["Bag".to_string()]), "t.json").unwrap();
        let reimported: HashMap<String, TypeDecl> =
            decls.iter().map(|t| (t.name.clone(), t.clone())).collect();
        let reemitted = crate::types::json_schema_string(&reimported["Bag"], &reimported);
        assert_eq!(emitted, reemitted, "map schema round-trip must be exact");
        // The imported field is a `Map<String, Int64>`.
        let bag = decls.iter().find(|d| d.name == "Bag").unwrap();
        let Type::Record(fields) = &bag.base else {
            panic!("record")
        };
        // The field's synthetic type resolves to a Map with a String key.
        let fty = crate::types::resolve(&fields[0].ty, &reimported);
        assert!(
            matches!(&fty, Type::Map(k, v) if **k == Type::Str && **v == Type::Int),
            "{fty:?}"
        );
    }

    /// A `{"enum": [..]}` schema imports as a payload-less Vyrn enum, and the
    /// emitter's enum encoding round-trips byte-exactly.
    #[test]
    fn imports_enum_schemas_and_round_trips() {
        let decls = synthesize(
            r#"{"title": "Color", "enum": ["Red", "Green", "Blue"]}"#,
            Some(&["Color".to_string()]),
            "t.json",
        )
        .unwrap();
        let color = decls.iter().find(|d| d.name == "Color").unwrap();
        match &color.base {
            Type::Enum(vs) => {
                assert_eq!(
                    vs.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(),
                    ["Red", "Green", "Blue"]
                );
                assert!(vs.iter().all(|v| v.payload.is_empty()));
            }
            other => panic!("expected an enum, got {other:?}"),
        }
        // Round trip: emit from Vyrn, import, re-emit — byte-equal.
        let src = "type Color = | Red | Green | Blue\nfn main() -> Int64 { return 0 }";
        let program = crate::check(src).unwrap();
        let types: HashMap<String, TypeDecl> = program
            .type_decls
            .iter()
            .map(|t| (t.name.clone(), t.clone()))
            .collect();
        let emitted = crate::types::json_schema_string(&types["Color"], &types);
        let doc = emitted.replacen("{", "{\"title\":\"Color\",", 1);
        let decls = synthesize(&doc, Some(&["Color".to_string()]), "t.json").unwrap();
        let reimported: HashMap<String, TypeDecl> =
            decls.iter().map(|t| (t.name.clone(), t.clone())).collect();
        assert_eq!(
            emitted,
            crate::types::json_schema_string(&reimported["Color"], &reimported),
            "enum schema round-trip must be exact"
        );
    }

    /// A mixed nullary+payload enum (RFC-0024) emits a `oneOf`, imports back into
    /// an enum decl, and re-emits byte-identically (the pinned round-trip law).
    #[test]
    fn payload_enum_schema_round_trips() {
        let src = "type Shape = | Circle(Int64) | Rect(Int64, Int64) | Unit\n\
                   fn main() -> Int64 { return 0 }";
        let program = crate::check(src).unwrap();
        let types: HashMap<String, TypeDecl> = program
            .type_decls
            .iter()
            .map(|t| (t.name.clone(), t.clone()))
            .collect();
        let emitted = crate::types::json_schema_string(&types["Shape"], &types);
        assert!(emitted.contains("\"oneOf\""), "{emitted}");
        assert!(
            emitted.contains("\"prefixItems\""),
            "tuple payload: {emitted}"
        );
        assert!(emitted.contains("\"const\":\"Unit\""), "nullary: {emitted}");
        let doc = emitted.replacen("{", "{\"title\":\"Shape\",", 1);
        let decls = synthesize(&doc, Some(&["Shape".to_string()]), "t.json").unwrap();
        let reimported: HashMap<String, TypeDecl> =
            decls.iter().map(|t| (t.name.clone(), t.clone())).collect();
        // The imported base is a payload enum with the same variants/arities.
        match &reimported["Shape"].base {
            Type::Enum(vs) => {
                assert_eq!(vs[0].name, "Circle");
                assert_eq!(vs[0].payload.len(), 1);
                assert_eq!(vs[1].name, "Rect");
                assert_eq!(vs[1].payload.len(), 2);
                assert_eq!(vs[2].name, "Unit");
                assert!(vs[2].payload.is_empty());
            }
            other => panic!("expected enum, got {other:?}"),
        }
        assert_eq!(
            emitted,
            crate::types::json_schema_string(&reimported["Shape"], &reimported),
            "payload-enum schema round-trip must be exact"
        );
    }

    /// `Result<T, E>` inside a record emits the `Ok`/`Err` `oneOf`, imports as a
    /// two-variant payload enum, and re-emits byte-identically.
    #[test]
    fn result_in_record_schema_round_trips() {
        let src = "type User = { id: Int64, name: String }\n\
                   type Resp = { outcome: Result<User, String> }\n\
                   fn main() -> Int64 { return 0 }";
        let program = crate::check(src).unwrap();
        let types: HashMap<String, TypeDecl> = program
            .type_decls
            .iter()
            .map(|t| (t.name.clone(), t.clone()))
            .collect();
        let emitted = crate::types::json_schema_string(&types["Resp"], &types);
        assert!(
            emitted.contains("\"properties\":{\"Ok\":{\"$ref\":\"#/$defs/User\"}}"),
            "{emitted}"
        );
        assert!(
            emitted.contains("\"properties\":{\"Err\":{\"type\":\"string\"}}"),
            "{emitted}"
        );
        let doc = emitted.replacen("{", "{\"title\":\"Resp\",", 1);
        let decls = synthesize(&doc, Some(&["Resp".to_string()]), "t.json").unwrap();
        let reimported: HashMap<String, TypeDecl> =
            decls.iter().map(|t| (t.name.clone(), t.clone())).collect();
        assert_eq!(
            emitted,
            crate::types::json_schema_string(&reimported["Resp"], &reimported),
            "Result-in-record schema round-trip must be exact"
        );
    }

    /// Non-string `enum` entries and extra keywords alongside `enum` are hard
    /// errors (exactly-or-not-at-all).
    #[test]
    fn enum_schema_rejects_non_strings_and_extras() {
        let err = synthesize(
            r#"{"title": "Bad", "enum": ["A", 3]}"#,
            Some(&["Bad".to_string()]),
            "t.json",
        )
        .unwrap_err();
        assert!(err.contains("`enum` entries must all be strings"), "{err}");
        let err = synthesize(
            r#"{"title": "Bad", "enum": ["A"], "type": "string"}"#,
            Some(&["Bad".to_string()]),
            "t.json",
        )
        .unwrap_err();
        assert!(
            err.contains("unsupported keyword `type` alongside `enum`"),
            "{err}"
        );
    }

    /// A recursive type round-trips through its `$ref` back-edge: the emitter
    /// renders `next: Option<Node>` as `{"$ref":"#"}`, the importer resolves
    /// `#` to the root title, and re-emission is byte-identical.
    #[test]
    fn recursive_type_round_trips_via_root_ref() {
        let src = "type Node = { name: String, next: Option<Node> }\n\
                   fn main() -> Int64 { return 0 }";
        let program = crate::check(src).unwrap();
        let types: HashMap<String, TypeDecl> = program
            .type_decls
            .iter()
            .map(|t| (t.name.clone(), t.clone()))
            .collect();
        let emitted = crate::types::json_schema_string(&types["Node"], &types);
        assert!(emitted.contains("\"next\":{\"$ref\":\"#\"}"), "{emitted}");
        let doc = emitted.replacen("{", "{\"title\":\"Node\",", 1);
        let decls = synthesize(&doc, Some(&["Node".to_string()]), "t.json").unwrap();
        let reimported: HashMap<String, TypeDecl> =
            decls.iter().map(|t| (t.name.clone(), t.clone())).collect();
        assert_eq!(
            emitted,
            crate::types::json_schema_string(&reimported["Node"], &reimported),
            "recursive schema round-trip must be exact"
        );
    }

    /// A sized-int type emits its width bounds and round-trips: the import
    /// synthesizes an Int64 + `where` refinement whose re-emission is
    /// byte-identical (the wire contract is the bounds, not the Rust width).
    #[test]
    fn sized_int_bounds_round_trip() {
        let src = "type Byte = UInt8\nfn main() -> Int64 { return 0 }";
        let program = crate::check(src).unwrap();
        let types: HashMap<String, TypeDecl> = program
            .type_decls
            .iter()
            .map(|t| (t.name.clone(), t.clone()))
            .collect();
        let emitted = crate::types::json_schema_string(&types["Byte"], &types);
        assert!(
            emitted.contains("\"minimum\":0,\"maximum\":255"),
            "{emitted}"
        );
        let doc = emitted.replacen("{", "{\"title\":\"Byte\",", 1);
        let decls = synthesize(&doc, Some(&["Byte".to_string()]), "t.json").unwrap();
        let reimported: HashMap<String, TypeDecl> =
            decls.iter().map(|t| (t.name.clone(), t.clone())).collect();
        assert_eq!(
            emitted,
            crate::types::json_schema_string(&reimported["Byte"], &reimported),
            "sized-int schema round-trip must be exact"
        );
    }

    /// A raw control byte inside a string literal is invalid JSON (RFC 8259
    /// §7) — the same rule codec.rs enforces — so a manifest carrying one is
    /// refused with a named error, not silently accepted.
    #[test]
    fn a_raw_control_byte_in_a_string_is_refused() {
        assert!(parse_json("\"ok \\u0001 escaped\"").is_ok());
        let e = parse_json("\"bad \u{1} raw\"").unwrap_err();
        assert!(e.contains("control character"), "{e}");
    }

    /// A fractional `minLength` ceils (lengths are whole numbers: `2.5` means
    /// ≥ 3), and a negative one is a hard error — an imported type is never
    /// silently weaker than its schema.
    #[test]
    fn fractional_min_length_ceils_and_negative_is_an_error() {
        let decls = synth(r#"{"title": "S", "type": "string", "minLength": 2.5}"#);
        let pred = crate::checker::pred_summary(decls[0].predicate.as_ref().unwrap());
        assert_eq!(pred, "value.byteLength >= 3");

        let e = synthesize(
            r#"{"title": "S", "type": "string", "minLength": -1}"#,
            None,
            "t.json",
        )
        .unwrap_err();
        assert!(e.contains("`minLength` -1 is negative"), "{e}");
    }

    /// Malformed `properties` and `required` entries naming no property are
    /// hard errors: the old code read a non-object `properties` as "no fields"
    /// and dropped dangling `required` names, importing `{}` for a schema that
    /// said something entirely different.
    #[test]
    fn malformed_properties_and_dangling_required_are_hard_errors() {
        let e = synthesize(
            r#"{"title": "X", "type": "object", "properties": [1, 2]}"#,
            None,
            "t.json",
        )
        .unwrap_err();
        assert!(e.contains("`properties` must be an object"), "{e}");

        let e = synthesize(
            r#"{"title": "X", "type": "object",
                "properties": {"a": {"type": "boolean"}},
                "required": ["a", "ghost", "phantom"]}"#,
            None,
            "t.json",
        )
        .unwrap_err();
        assert!(e.contains("`ghost`, `phantom`"), "{e}");
    }

    /// A minimum of exactly `i64::MIN` bounds at `i64::MIN`: the old
    /// `Expr::Int(-n as i64)` under a `Unary::Neg` saturated `-n` at
    /// `i64::MAX` (Rust unary minus binds tighter than `as`) and imported
    /// `value >= -9223372036854775807`, rejecting the one value the schema
    /// admits at the boundary. The bound is now a plain negative literal,
    /// which reflection (`predicate_bounds`) reads too.
    #[test]
    fn an_i64_min_minimum_bounds_at_i64_min() {
        let decls = synth(r#"{"title": "T", "type": "integer", "minimum": -9223372036854775808}"#);
        let pred = crate::checker::pred_summary(decls[0].predicate.as_ref().unwrap());
        assert_eq!(pred, "value >= -9223372036854775808");
        // And the ordinary negative case keeps its exact value.
        let decls = synth(r#"{"title": "T", "type": "integer", "minimum": -5}"#);
        let pred = crate::checker::pred_summary(decls[0].predicate.as_ref().unwrap());
        assert_eq!(pred, "value >= -5");
    }
}
