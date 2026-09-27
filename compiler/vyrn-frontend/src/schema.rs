//! JSON Schema type imports.
//!
//! `import type { User } from "./api.schema.json"` synthesizes [`TypeDecl`]s
//! from a JSON Schema document, the inverse of the `jsonSchema(T)` emitter in
//! [`crate::types`]: numeric keywords become `where` clauses over `value`,
//! string keywords become string refinements, `object` with `required` becomes
//! a record with `Option<T>` for optional fields, and constrained inline shapes
//! become synthetic named types (`User.address`). Anything the mapping cannot
//! express is an error naming the type and keyword, so an imported type is
//! never weaker than its schema; informational keywords are ignored.

use crate::ast::*;

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    /// Insertion-ordered: field order is record layout.
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

/// Nesting limit for `{` and `[`.
///
/// The parser recurses, and `find_manifest` reads a `vyrn.json` from every
/// ancestor of the working directory, which the user may not own. Without a
/// bound, one deep manifest ends every `vyrn` command in a stack overflow. The
/// value matches `std/jsonread` and `std/von`: about two frames per level
/// against the smallest stack of any build profile.
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
    /// Objects and arrays open at this point.
    depth: usize,
}

impl P<'_> {
    fn ws(&mut self) {
        // JSON whitespace is space, tab, LF and CR (RFC 8259 section 2);
        // `char::is_whitespace` would admit NBSP and U+2028, which no other reader
        // accepts.
        while self.i < self.b.len() && matches!(self.b[self.i], ' ' | '\t' | '\n' | '\r') {
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
    /// Enters one nesting level, or refuses. Every recursive call goes through
    /// here.
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
        // A side set keeps the duplicate check linear: a manifest above the working
        // directory with ~100k keys would otherwise hang every command.
        let mut seen = std::collections::HashSet::new();
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
            // A repeated name has two meanings: `Json::get` reads the first pair and the
            // `$defs` walk keeps the last. `std/von` and `std/jsonread` refuse it too.
            if !seen.insert(k.clone()) {
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
                            // Surrogate pairs are refused rather than mis-decoded.
                            let ch = char::from_u32(cp)
                                .ok_or("surrogate \\u escapes are not supported")?;
                            out.push(ch);
                        }
                        other => return Err(format!("unknown escape \\{other}")),
                    }
                }
                Some(c) => {
                    // A raw control character is invalid in a JSON string (RFC 8259 section
                    // 7), as in `crate::codec`.
                    if c < '\u{20}' {
                        return Err(format!("control character in string at offset {}", self.i));
                    }
                    out.push(c);
                    self.i += 1;
                }
            }
        }
    }
    /// Parses the RFC 8259 number grammar: `-? (0 | [1-9][0-9]*)`, then an
    /// optional fraction and exponent, each with its own digits. A leading zero
    /// (`01`) and a bare trailing dot (`1.`) are refused, as `std/jsonread` and
    /// [`crate::codec`] refuse them.
    fn num(&mut self) -> Result<Json, String> {
        let start = self.i;
        if self.peek() == Some('-') {
            self.i += 1;
        }
        let bad = || format!("bad number at offset {start}");
        match self.peek() {
            Some('0') => self.i += 1,
            Some(c) if c.is_ascii_digit() => {
                while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    self.i += 1;
                }
            }
            _ => return Err(bad()),
        }
        if self.peek() == Some('.') {
            self.i += 1;
            if !self.peek().is_some_and(|c| c.is_ascii_digit()) {
                return Err(bad());
            }
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.i += 1;
            }
        }
        if matches!(self.peek(), Some('e') | Some('E')) {
            self.i += 1;
            if matches!(self.peek(), Some('+') | Some('-')) {
                self.i += 1;
            }
            if !self.peek().is_some_and(|c| c.is_ascii_digit()) {
                return Err(bad());
            }
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.i += 1;
            }
        }
        let text: String = self.b[start..self.i].iter().collect();
        text.parse::<f64>()
            .map(Json::Num)
            .map_err(|_| format!("bad number `{text}`"))
    }
}

/// Synthesizes the requested types, and everything they reach through `$defs`
/// and nested objects, from a JSON Schema document. `module` is the resolved
/// path. Requested names are exported; the helpers they pull in are not.
pub fn synthesize(
    source: &str,
    requested: Option<&[String]>,
    module: &str,
) -> Result<Vec<TypeDecl>, String> {
    let doc = parse_json(source).map_err(|e| format!("{module}: invalid JSON: {e}"))?;
    let mut out: Vec<TypeDecl> = Vec::new();

    // A name is the root (matching `title`) or a `$defs` key.
    let defs = doc.get("$defs");
    let root_title = doc.get("title").and_then(|t| t.as_str());

    let mut pending: Vec<(String, &Json, bool)> = Vec::new();
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
        // With no list, synthesize everything; the import machinery filters by the
        // names imported.
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
        // Queue the `$defs` targets of `$ref`. `$ref: "#"` names the root, which
        // sits at the document top, not under `$defs`.
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
            line: 1, // line 0 means "parser-injected" and is deduplicated away
        });
        // The `Parent.field` helpers for constrained inline fields and elements.
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

/// Keywords ignored everywhere.
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

/// Converts one schema object to its base type, predicate, and the `$defs`
/// names it references.
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

    if let Some(r) = schema.get("$ref").and_then(|r| r.as_str()) {
        // `#` is the root: the back-edge the emitter writes for a self-referential
        // type (`next: Option<Node>`).
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

    // An `enum` of strings is a payload-less enum, the inverse of the emitter.
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

    // A `oneOf` of externally tagged members is a payload enum, the
    // inverse of the emitter.
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
                // JSON Schema requires `multipleOf > 0`; a zero would import as
                // `value % 0 == 0`, which traps on every validation.
                if *k <= 0.0 {
                    return Err(format!(
                        "{module}: type `{name}`: `multipleOf` {k} is not positive"
                    ));
                }
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
                // A fractional minimum ceils (`2.5` admits length 3 and up); truncating
                // would import a weaker type. A negative one is an error.
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
                // Constrained elements become a synthetic validated type, so every element
                // validates at its boundaries.
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
            // An `additionalProperties` object is a `Map<String, V>`. Vyrn
            // has no record with an index signature, so it carries no `properties`.
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
                    // Constrained values become a synthetic validated type, as array items do.
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
                // A non-object is a typo; reading it as "no fields" would import a weaker
                // type.
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
            // A `required` entry naming no property is a presence check the record
            // cannot carry (decode ignores unknown fields).
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
                // A constrained or nested-object field becomes a synthetic named type, so
                // the emitter's inline constraints round-trip. Only a required one: an
                // optional one would be `Option<Parent.field>`, which no Vyrn source can
                // write (an `Option` alias cannot have a `where`).
                if (fpred.is_some() || matches!(fty, Type::Record(_)))
                    && !required.contains(&fname.as_str())
                {
                    return Err(format!(
                        "{module}: type `{name}`: optional property `{fname}` cannot \
                         carry its own constraints: make it `required` or name the \
                         shape in `$defs` (Vyrn imports schemas exactly or not at all)"
                    ));
                }
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
                    Type::option(fty)
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

/// Converts one `oneOf` member to a variant. `{"const":"Name"}` is
/// nullary; a single-property object `{"Name": <sub>}` with `required: ["Name"]`
/// carries one payload, or a tuple through
/// `{"type":"array","prefixItems":[..],"items":false}`.
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
        let sub_name = format!("{name}.{vname}");
        let pty = convert_payload_type(sub, &sub_name, module, root, nested, refs)?;
        return Ok(crate::ast::EnumVariant {
            name: vname.clone(),
            payload: vec![pty],
        });
    }
    Err(bad())
}

/// Converts an enum payload schema, promoting a constrained or nested shape to
/// a synthetic named type as record fields do.
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
        // Negate in Rust, not through the AST: `-n as i64` saturates at `i64::MAX`,
        // and a `Unary::Neg` hides the literal from `predicate_bounds`. `i64::MIN` is
        // the one value whose negation overflows.
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

    /// The limit is read from the code, so the test follows the parser.
    #[test]
    fn a_document_deeper_than_the_limit_is_refused_not_crashed() {
        let nest = |d: usize| format!("{}{}", "[".repeat(d), "]".repeat(d));
        assert!(
            parse_json(&nest(MAX_JSON_DEPTH)).is_ok(),
            "at the limit is inside it"
        );
        let e = parse_json(&nest(MAX_JSON_DEPTH + 1)).unwrap_err();
        assert!(e.contains(&MAX_JSON_DEPTH.to_string()), "{e}");
        // Objects nest through the same check, and so does a mixture.
        let obj = format!(
            "{}1{}",
            "{\"a\":".repeat(MAX_JSON_DEPTH + 1),
            "}".repeat(MAX_JSON_DEPTH + 1)
        );
        assert!(parse_json(&obj).is_err());
        // Depth is not length: 100k siblings at one level are fine.
        let wide = format!("[{}]", vec!["1"; 100_000].join(","));
        assert!(parse_json(&wide).is_ok());
    }

    /// `get` reads the first pair and the `$defs` walk the last, so a repeated
    /// key is refused.
    #[test]
    fn a_name_defined_twice_is_refused() {
        let e = parse_json(r#"{"main":"a.vyrn","main":"b.vyrn"}"#).unwrap_err();
        assert!(e.contains("main"), "names the key: {e}");
        assert!(
            parse_json(r#"{"a":{"x":1},"b":{"x":2}}"#).is_ok(),
            "a repeat in a SIBLING object is a different name"
        );
    }

    /// A leading zero, a bare trailing dot and NBSP are refused, as the strict
    /// readers (`std/jsonread`, [`crate::codec`]) refuse them.
    #[test]
    fn numbers_and_whitespace_are_strict_json() {
        assert_eq!(parse_json("[0, -0.5, 1e2, 17E+3]").unwrap(), {
            Json::Arr(vec![
                Json::Num(0.0),
                Json::Num(-0.5),
                Json::Num(100.0),
                Json::Num(17000.0),
            ])
        });
        for bad in [
            "[01]",
            "[-007]",
            "[1.]",
            "[.5]",
            "[1e]",
            "[1e+]",
            "{ \"a\":1\u{a0}}",
        ] {
            let e = parse_json(bad).unwrap_err();
            assert!(!e.is_empty(), "{bad} parses");
        }
    }

    /// A zero divisor would trap on every validation.
    #[test]
    fn a_non_positive_multiple_of_is_a_hard_error() {
        for k in ["0", "-2"] {
            let e = synthesize(
                &format!(r#"{{"title": "T", "type": "integer", "multipleOf": {k}}}"#),
                None,
                "t.json",
            )
            .unwrap_err();
            assert!(e.contains("multipleOf"), "{k}: {e}");
        }
    }

    /// An optional constrained property would import as `Option<Parent.field>`,
    /// which no Vyrn source can write.
    #[test]
    fn an_optional_constrained_property_is_a_hard_error() {
        for prop in [
            r#"{"type": "string", "minLength": 3}"#,
            r#"{"type": "object", "properties": {"street": {"type": "string"}}}"#,
        ] {
            let e = synthesize(
                &format!(
                    r#"{{"title": "User", "type": "object",
                        "properties": {{"nick": {prop}}}}}"#
                ),
                None,
                "t.json",
            )
            .unwrap_err();
            assert!(e.contains("nick"), "{prop}: {e}");
        }
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
        assert_eq!(fields[1].ty, Type::option(Type::Str));
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
        // Emit, import, re-emit: byte-equal.
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
        // The importer binds the root by its title.
        let doc = emitted.replacen("{", "{\"title\":\"User\",", 1);

        let decls = synthesize(&doc, Some(&["User".to_string()]), "t.json").unwrap();
        let reimported: HashMap<String, TypeDecl> =
            decls.iter().map(|t| (t.name.clone(), t.clone())).collect();
        let reemitted = crate::types::json_schema_string(&reimported["User"], &reimported);
        assert_eq!(emitted, reemitted, "schema round-trip must be exact");
    }

    /// A `Map<String, V>` field round-trips through `additionalProperties`.
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
        // Emit, import, re-emit: byte-equal.
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

    /// A mixed nullary and payload enum round-trips through `oneOf`.
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

    /// `Result<T, E>` in a record round-trips through the `Ok`/`Err` `oneOf`.
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

    /// A recursive type round-trips through its `$ref: "#"` back-edge.
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

    /// A sized integer round-trips as `Int64` with width bounds: the wire contract
    /// is the bounds, not the width.
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

    #[test]
    fn a_raw_control_byte_in_a_string_is_refused() {
        assert!(parse_json("\"ok \\u0001 escaped\"").is_ok());
        let e = parse_json("\"bad \u{1} raw\"").unwrap_err();
        assert!(e.contains("control character"), "{e}");
    }

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

    /// `-n as i64` saturates at `i64::MAX`, which would reject the one boundary
    /// value the schema admits.
    #[test]
    fn an_i64_min_minimum_bounds_at_i64_min() {
        let decls = synth(r#"{"title": "T", "type": "integer", "minimum": -9223372036854775808}"#);
        let pred = crate::checker::pred_summary(decls[0].predicate.as_ref().unwrap());
        assert_eq!(pred, "value >= -9223372036854775808");
        let decls = synth(r#"{"title": "T", "type": "integer", "minimum": -5}"#);
        let pred = crate::checker::pred_summary(decls[0].predicate.as_ref().unwrap());
        assert_eq!(pred, "value >= -5");
    }
}
