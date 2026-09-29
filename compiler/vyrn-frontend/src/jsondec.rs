//! The type-directed JSON decode walk, built once for every
//! backend.
//!
//! `fromJson(T, s)` is `std/jsonread`'s `read(s)`, which needs no compiler,
//! then `decode(tree, T)`, which this module generates: Vyrn functions that
//! walk a `std/json` `Json` value. It is `std/json`'s `jsonEncoders` run
//! backwards, with the same two mechanisms: generated source handed to the
//! parser, and one function per distinct type so a self-referential type
//! terminates.
//!
//! A decoder returns `Array<T>` with zero or one element, not `Option<T>`.
//! Decode accumulates issues and constructs a composite only when
//! every part succeeded, and `Option<U>` is itself a decode target whose
//! `Option<Option<U>>` has no wire form. At the use site,
//! `for x in dec(..) { .. }` runs exactly when a value was produced.
//!
//! A refined type's predicate is not lowered here: a synthesized
//! `Bool`-returning function takes the predicate's own AST as its body and
//! [`crate::types::predicate_binds`] as its parameters, the structure the trap
//! path binds, so decode and trap run the same expression.

use std::collections::HashMap;

use crate::ast::{Block, Capability, Function, Id, Param, Stmt, Type, TypeDecl};
use crate::codec::Wire;

/// Placeholder prefix for a name in the injected `std/json`.
const PH: &str = "VyrnRt_";
/// Placeholder prefix for a name in the injected `std/jsondec`. Each runtime
/// module is renamed to its own reserved prefix, so each needs its own.
const PD: &str = "VyrnRd_";

/// The reserved prefix of `std/jsondec`'s declarations.
fn rd_prefix() -> &'static str {
    "jsondec$"
}

/// The type's structural identity, [`crate::types::struct_key`], shared with
/// the `TypeArg` nodes and the codegen symbols. A readable mangle once let two
/// instantiations collide, and a decoder picked by a colliding name decodes
/// the wrong shape.
fn type_key(ty: &Type) -> String {
    crate::types::struct_key(ty)
}

/// Returns the reserved name of the `String -> Validation<T>` entry point for
/// a decode target; `fromJson<T>(s)` calls it
/// ([`crate::loader::routed_callee`]).
pub fn top_name(ty: &Type) -> String {
    format!("{}t{}", crate::loader::RT_PREFIX, type_key(ty))
}

fn top_ph(ty: &Type) -> String {
    format!("{PH}t{}", type_key(ty))
}

fn dec_ph(ty: &Type) -> String {
    format!("{PH}d{}", type_key(ty))
}

fn pred_ph(ty: &Type) -> String {
    format!("{PH}p{}", type_key(ty))
}

/// Spells a type as generated source, with the injected modules' `$` names
/// folded onto placeholders.
fn spell(ty: &Type) -> String {
    ty.to_string()
        .replace(crate::loader::RT_PREFIX, PH)
        .replace(rd_prefix(), PD)
}

/// Generates the decoders for the `fromJson` targets `tys` and everything they
/// reach, to append to a linked `Program`.
///
/// A type the walk cannot decode is skipped: `fromJson` refuses it at the call
/// site (`crate::codec::decodable`), and a program that never decodes it must
/// not fail.
pub fn decoders(
    tys: &[Type],
    types: &HashMap<String, TypeDecl>,
) -> Result<(Vec<Function>, Vec<TypeDecl>), String> {
    let mut w = Walk {
        types,
        done: HashMap::new(),
        source: String::new(),
        names: Vec::new(),
        preds: Vec::new(),
        aliases: Vec::new(),
    };
    for ty in tys {
        let _ = w.top(ty);
    }
    if w.source.is_empty() {
        return Ok((w.preds, w.aliases));
    }
    w.parse()
}

struct Walk<'a> {
    types: &'a HashMap<String, TypeDecl>,
    /// Names emitted or in progress. A name is inserted before its body is built,
    /// which makes a self-referential type terminate. The type is kept beside
    /// the name because [`crate::types::struct_key`] truncates to 64 bits: two
    /// types on one name are a build error, not one shared decoder.
    done: HashMap<String, Type>,
    source: String,
    /// Every placeholder the source mentions, for the rename map.
    names: Vec<String>,
    /// Predicate functions, built as AST: the `where` expression has no source
    /// spelling.
    preds: Vec<Function>,
    /// Aliases for anonymous record types. `{ c: 1 }` is not an expression, only
    /// `T { c: 1 }` is, so a nested inline record type gets one transparent alias
    /// per shape, used only for the literal.
    aliases: Vec<TypeDecl>,
}

impl Walk<'_> {
    /// Claims `ph` for `ty`; `Ok(false)` if `ty` already holds it. `Err` is a
    /// [`crate::types::struct_key`] truncation collision: two types on one name.
    fn reserve(&mut self, ph: &str, ty: &Type) -> Result<bool, String> {
        if let Some(prev) = self.done.get(ph) {
            if prev != ty {
                return Err(format!(
                    "internal: {prev} and {ty} share the decoder name `{ph}`"
                ));
            }
            return Ok(false);
        }
        self.done.insert(ph.to_string(), ty.clone());
        self.names.push(ph.to_string());
        Ok(true)
    }

    /// Ensures the entry point for a decode target exists.
    fn top(&mut self, ty: &Type) -> Result<String, String> {
        let ph = top_ph(ty);
        if !self.reserve(&ph, ty)? {
            return Ok(ph);
        }
        let d = self.decoder(ty)?;
        let (rd, t) = (self.rd("readDoc"), spell(ty));
        // Walk the doc by `consume` and return the accumulator by `consume`: the
        // plain walk leaked the doc buffer and the decoded snapshot per `fromJson`.
        self.source.push_str(&format!(
            "fn {ph}(src: String) -> Validation<{t}> {{\n\
             \x20   let mut iss: Array<Issue> = []\n\
             \x20   let doc = {rd}(src, iss)\n\
             \x20   let mut val: Array<{t}> = []\n\
             \x20   for j in consume doc {{\n\
             \x20       val = {d}(j, \"\", iss)\n\
             \x20   }}\n\
             \x20   if iss.length > 0 {{\n\
             \x20       return Invalid(consume iss)\n\
             \x20   }}\n\
             \x20   for x in consume val {{\n\
             \x20       return Valid(x)\n\
             \x20   }}\n\
             \x20   return Invalid(consume iss)\n\
             }}\n"
        ));
        Ok(ph)
    }

    /// Ensures a decoder for `ty` exists and returns its placeholder name.
    fn decoder(&mut self, ty: &Type) -> Result<String, String> {
        if matches!(ty, Type::Enum(_)) && !crate::types::is_sum_alias(ty) {
            // An anonymous enum has no source spelling, so it cannot be a return type.
            // Declared enums arrive as `Type::Named`, and `Option` and `Result` are
            // spelled by `Display`.
            return Err("fromJson: cannot decode an anonymous enum".to_string());
        }
        let ph = dec_ph(ty);
        if !self.reserve(&ph, ty)? {
            return Ok(ph);
        }
        let body = self.body(ty)?;
        let (json, t) = (self.rt("Json"), spell(ty));
        self.source.push_str(&format!(
            "fn {ph}(v: {json}, path: String, iss: modify Array<Issue>) -> Array<{t}> {{\n{body}}}\n"
        ));
        Ok(ph)
    }

    /// Registers a placeholder for a `std/json` name and returns it.
    fn rt(&mut self, name: &str) -> String {
        let ph = format!("{PH}{name}");
        if !self.names.contains(&ph) {
            self.names.push(ph.clone());
        }
        ph
    }

    /// Registers a placeholder for a `std/jsondec` name and returns it.
    fn rd(&mut self, name: &str) -> String {
        let ph = format!("{PD}{name}");
        if !self.names.contains(&ph) {
            self.names.push(ph.clone());
        }
        ph
    }

    /// Returns the body of `ty`'s decoder. [`crate::codec::wire`] decides what a
    /// type is on the wire, for the checker, the schema emitter and the
    /// `TypeArg` builder alike; this spells it backwards.
    fn body(&mut self, ty: &Type) -> Result<String, String> {
        // A named type stays unresolved: a refinement decodes its base, then runs its
        // `where` clause.
        if let Type::Named(n) = ty {
            let decl = self
                .types
                .get(n)
                .ok_or_else(|| format!("fromJson: unknown type `{n}`"))?
                .clone();
            if decl.predicate.is_some() {
                return self.refined_body(ty, &decl);
            }
        }
        let t = spell(ty);
        match crate::codec::wire(ty, self.types, true)
            .map_err(|o| format!("fromJson: cannot decode {o}"))?
        {
            Wire::Int => Ok(self.scalar("dInt64", "")),
            Wire::IntN { bits, signed } if signed => {
                let (lo, hi) = signed_bounds(bits);
                Ok(self.narrow(&t, "dIntRange", &format!(", {lo}, {hi}")))
            }
            Wire::IntN { bits, .. } => {
                let hi = unsigned_max(bits);
                if bits == 64 {
                    Ok(self.scalar("dUIntMax", &format!(", {hi}")))
                } else {
                    Ok(self.narrow(&t, "dUIntMax", &format!(", {hi}")))
                }
            }
            Wire::Float => Ok(self.scalar("dFloat64", "")),
            Wire::Float32 => Ok(self.scalar("dFloat32", "")),
            Wire::Bool => Ok(self.scalar("dBool", "")),
            Wire::Str => Ok(self.scalar("dStr", "")),
            Wire::Record(fields) => {
                // The literal needs a name: an anonymous record gets a synthesized alias.
                let lit = match ty {
                    Type::Named(_) => t.clone(),
                    _ => self.rec_alias(&fields),
                };
                self.record_body(&t, &lit, &fields)
            }
            Wire::Array(inner) => self.array_body(&t, &inner),
            Wire::Map(val) => self.map_body(&t, &val),
            Wire::MapI(val) => self.map_body_i(&t, &val),
            Wire::Option(inner) => self.option_body(&t, &inner),
            // `Result<T, E>` arrives as its two-variant enum, so external tagging and its
            // "one of `Ok`, `Err`" message come from one place.
            Wire::Enum(vs) => {
                let expected = crate::codec::enum_expected(&vs);
                self.variants_body(&t, &vs, &expected)
            }
            // `wire` refuses a fixed array in the decode direction: its length is
            // unknown until the data arrives.
            Wire::FixedArray(..) => Err(format!("fromJson: cannot decode {ty}")),
        }
    }

    /// A scalar whose helper answers in the target type.
    fn scalar(&mut self, helper: &str, extra: &str) -> String {
        let h = self.rd(helper);
        format!("    return {h}(v, path, iss{extra})\n")
    }

    /// A sized integer: the helper answers in `Int64` or `UInt64` and refuses
    /// every out-of-range value, so the narrowing conversion cannot wrap.
    fn narrow(&mut self, t: &str, helper: &str, extra: &str) -> String {
        let h = self.rd(helper);
        format!(
            "    let mut out: Array<{t}> = []\n\
             \x20   for n in {h}(v, path, iss{extra}) {{\n\
             \x20       out.push({t}(n))\n\
             \x20   }}\n\
             \x20   return out\n"
        )
    }

    /// A refined type: decode the base, run the predicate, and construct only when
    /// it holds. The push into `Array<Named>` is the construction: an element
    /// store validates, as `Age(n)` does, and also works for a record base, which
    /// has no `Name(value)` form.
    fn refined_body(&mut self, ty: &Type, decl: &TypeDecl) -> Result<String, String> {
        let base = self.decoder(&decl.base)?;
        let pred = self.predicate(ty, decl);
        let args: Vec<String> = crate::types::predicate_binds(decl)
            .into_iter()
            .map(|(name, _, field)| match field {
                Some(_) => format!("b.{name}"),
                None => "b".to_string(),
            })
            .collect();
        let pv = self.rd("pushValidate");
        let msg = crate::codec::validate_message(decl);
        let t = spell(ty);
        Ok(format!(
            "    let mut out: Array<{t}> = []\n\
             \x20   for b in {base}(v, path, iss) {{\n\
             \x20       if {pred}({}) {{\n\
             \x20           out.push(b)\n\
             \x20       }} else {{\n\
             \x20           {pv}(iss, path, \"{msg}\")\n\
             \x20       }}\n\
             \x20   }}\n\
             \x20   return out\n",
            args.join(", ")
        ))
    }

    /// Returns the function whose body is the `where` clause, built as AST, with
    /// the [`crate::types::predicate_binds`] parameters the trap path binds.
    ///
    /// It needs no collision check: a predicate's name is keyed on the same type
    /// as its decoder, so a collision already stopped the build at `dec_ph`.
    fn predicate(&mut self, ty: &Type, decl: &TypeDecl) -> String {
        let ph = pred_ph(ty);
        if self.done.contains_key(&ph) {
            return ph;
        }
        self.done.insert(ph.clone(), ty.clone());
        self.names.push(ph.clone());
        self.preds.push(Function {
            name: ph.clone(),
            exported: false,
            module: None,
            doc: None,
            type_params: Vec::new(),
            type_bounds: HashMap::new(),
            params: crate::types::predicate_binds(decl)
                .into_iter()
                .map(|(name, ty, _)| Param {
                    id: Id::NEW,
                    name,
                    capability: Capability::Read,
                    ty,
                    line: 0,
                    col: 0,
                })
                .collect(),
            ret: Type::Bool,
            body: Block {
                id: Id::NEW,
                stmts: vec![Stmt::Return {
                    id: Id::NEW,
                    value: Some(decl.predicate.clone().expect("predicate present")),
                    line: 0,
                }],
            },
            line: 0,
            col: 0,
            is_extern: false,
            is_export_extern: false,
            is_gen: false,
            is_mut: false,
        });
        ph
    }

    /// Returns the alias for an anonymous record type.
    fn rec_alias(&mut self, fields: &[crate::ast::Field]) -> String {
        let ty = Type::Record(fields.to_vec());
        let ph = format!("{PH}r{}", type_key(&ty));
        if !self.names.contains(&ph) {
            self.names.push(ph.clone());
            self.aliases.push(TypeDecl {
                name: ph.clone(),
                exported: false,
                module: None,
                doc: None,
                type_params: Vec::new(),
                base: ty,
                predicate: None,
                line: 0,
            });
        }
        ph
    }

    /// A record: each field decodes into its own staging array, and the record is
    /// built once every required field has a value. An `Option<T>` field is
    /// inlined: absent or `null` is `None`, and a failed inner decode leaves
    /// `None` with its issue recorded.
    fn record_body(
        &mut self,
        t: &str,
        lit_name: &str,
        fields: &[crate::ast::Field],
    ) -> Result<String, String> {
        let (kind, ptype, fields_of, has, at, fpath) = (
            self.rd("kindName"),
            self.rd("pushType"),
            self.rd("fieldsOf"),
            self.rd("hasField"),
            self.rd("fieldAt"),
            self.rd("fieldPath"),
        );
        let (missing, is_null) = (self.rd("pushMissing"), self.rd("isNull"));
        let mut out = format!(
            "    let mut out: Array<{t}> = []\n\
             \x20   let k = {kind}(v)\n\
             \x20   if k != \"object\" {{\n\
             \x20       {ptype}(iss, path, \"object\", k)\n\
             \x20       return out\n\
             \x20   }}\n\
             \x20   let fs = {fields_of}(v)\n"
        );
        let mut guards: Vec<String> = Vec::new();
        let mut inits: Vec<String> = Vec::new();
        for (i, f) in fields.iter().enumerate() {
            let name = &f.name;
            out.push_str(&format!("    let p{i} = {fpath}(path, \"{name}\")\n"));
            if let Some(inner) =
                crate::types::option_payload(&crate::types::resolve(&f.ty, self.types)).cloned()
            {
                let d = self.decoder(&inner)?;
                let ispell = spell(&inner);
                out.push_str(&format!(
                    "    let mut f{i}: Option<{ispell}> = None\n\
                     \x20   let j{i} = {at}(fs, \"{name}\")\n\
                     \x20   if {is_null}(j{i}) == false {{\n\
                     \x20       for x in {d}(j{i}, p{i}, iss) {{\n\
                     \x20           f{i} = Some(x)\n\
                     \x20       }}\n\
                     \x20   }}\n"
                ));
                inits.push(format!("{name}: f{i}"));
            } else {
                let d = self.decoder(&f.ty)?;
                let fspell = spell(&f.ty);
                out.push_str(&format!(
                    "    let mut f{i}: Array<{fspell}> = []\n\
                     \x20   if {has}(fs, \"{name}\") {{\n\
                     \x20       f{i} = {d}({at}(fs, \"{name}\"), p{i}, iss)\n\
                     \x20   }} else {{\n\
                     \x20       {missing}(iss, p{i}, \"{name}\")\n\
                     \x20   }}\n"
                ));
                guards.push(format!("f{i}.length == 1"));
                // Take the value out of its one-element carrier; see `payload_arm`.
                inits.push(format!("{name}: f{i}.swapRemove(0)"));
            }
        }
        let lit = format!("{lit_name} {{ {} }}", inits.join(", "));
        if guards.is_empty() {
            out.push_str(&format!("    out.push({lit})\n"));
        } else {
            out.push_str(&format!(
                "    if {} {{\n        out.push({lit})\n    }}\n",
                guards.join(" && ")
            ));
        }
        out.push_str("    return out\n");
        Ok(out)
    }

    /// An array: each element decodes at `path[i]`, and only produced values are
    /// kept. A bad element is an issue, not a missing array.
    fn array_body(&mut self, t: &str, inner: &Type) -> Result<String, String> {
        let d = self.decoder(inner)?;
        let (kind, ptype, items_of, elem, ipath) = (
            self.rd("kindName"),
            self.rd("pushType"),
            self.rd("itemsOf"),
            self.rd("elemAt"),
            self.rd("indexPath"),
        );
        let ispell = spell(inner);
        Ok(format!(
            "    let mut out: Array<{t}> = []\n\
             \x20   let k = {kind}(v)\n\
             \x20   if k != \"array\" {{\n\
             \x20       {ptype}(iss, path, \"array\", k)\n\
             \x20       return out\n\
             \x20   }}\n\
             \x20   let items = {items_of}(v)\n\
             \x20   let mut acc: Array<{ispell}> = []\n\
             \x20   let mut i = 0\n\
             \x20   while i < items.length {{\n\
             \x20       for e in {d}({elem}(items, i), {ipath}(path, i), iss) {{\n\
             \x20           acc.push(e)\n\
             \x20       }}\n\
             \x20       i = i + 1\n\
             \x20   }}\n\
             \x20   out.push(acc)\n\
             \x20   return out\n"
        ))
    }

    /// A `Map<String, V>` from any JSON object: document order becomes
    /// insertion order, and each value decodes at `path.<key>`. `std/jsonread`
    /// refuses repeated keys.
    ///
    /// The key is copied: a map takes its key, and `f` belongs to the snapshot
    /// `fieldsOf(v)` returns, which is released. Sharing the buffer freed the
    /// map's keys.
    fn map_body(&mut self, t: &str, val: &Type) -> Result<String, String> {
        let d = self.decoder(val)?;
        let (kind, ptype, fields_of, fpath) = (
            self.rd("kindName"),
            self.rd("pushType"),
            self.rd("fieldsOf"),
            self.rd("fieldPath"),
        );
        let vspell = spell(val);
        Ok(format!(
            "    let mut out: Array<{t}> = []\n\
             \x20   let k = {kind}(v)\n\
             \x20   if k != \"object\" {{\n\
             \x20       {ptype}(iss, path, \"object\", k)\n\
             \x20       return out\n\
             \x20   }}\n\
             \x20   let mut m: Map<String, {vspell}> = [:]\n\
             \x20   for f in {fields_of}(v) {{\n\
             \x20       let dv = {d}(f.value, {fpath}(path, f.key), iss)\n\
             \x20       if m.has(f.key) == false {{\n\
             \x20           for x in consume dv {{\n\
             \x20               m[f.key.copy()] = x\n\
             \x20           }}\n\
             \x20       }}\n\
             \x20   }}\n\
             \x20   out.push(m)\n\
             \x20   return out\n"
        ))
    }

    /// The `Map<Int64, V>` twin: `dIntKey` reads each key as canonical
    /// decimal text, or records an issue.
    fn map_body_i(&mut self, t: &str, val: &Type) -> Result<String, String> {
        let d = self.decoder(val)?;
        let (kind, ptype, fields_of, fpath, dkey) = (
            self.rd("kindName"),
            self.rd("pushType"),
            self.rd("fieldsOf"),
            self.rd("fieldPath"),
            self.rd("dIntKey"),
        );
        let vspell = spell(val);
        Ok(format!(
            "    let mut out: Array<{t}> = []\n\
             \x20   let k = {kind}(v)\n\
             \x20   if k != \"object\" {{\n\
             \x20       {ptype}(iss, path, \"object\", k)\n\
             \x20       return out\n\
             \x20   }}\n\
             \x20   let mut m: Map<Int64, {vspell}> = [:]\n\
             \x20   for f in {fields_of}(v) {{\n\
             \x20       for kn in {dkey}(f.key, {fpath}(path, f.key), iss) {{\n\
             \x20           let dv = {d}(f.value, {fpath}(path, f.key), iss)\n\
             \x20           if m.has(kn) == false {{\n\
             \x20               for x in consume dv {{\n\
             \x20                   m[kn] = x\n\
             \x20               }}\n\
             \x20           }}\n\
             \x20       }}\n\
             \x20   }}\n\
             \x20   out.push(m)\n\
             \x20   return out\n"
        ))
    }

    /// A bare `Option<T>`: `null` is `None`, anything else decodes the payload.
    fn option_body(&mut self, t: &str, inner: &Type) -> Result<String, String> {
        let d = self.decoder(inner)?;
        let is_null = self.rd("isNull");
        Ok(format!(
            "    let mut out: Array<{t}> = []\n\
             \x20   if {is_null}(v) {{\n\
             \x20       out.push(None)\n\
             \x20       return out\n\
             \x20   }}\n\
             \x20   for x in {d}(v, path, iss) {{\n\
             \x20       out.push(Some(x))\n\
             \x20   }}\n\
             \x20   return out\n"
        ))
    }

    /// A payload enum or `Result` in wire form: a bare string is a
    /// nullary variant, and a one-key object `{"Tag":..}` a payload variant (a
    /// tuple payload as an array). Any other form fails with the expected-one-of
    /// `json.type` issue.
    fn variants_body(
        &mut self,
        t: &str,
        vs: &[crate::ast::EnumVariant],
        expected: &str,
    ) -> Result<String, String> {
        let mut out = format!("    let mut out: Array<{t}> = []\n");
        if vs.iter().any(|v| v.payload.is_empty()) {
            let tag = self.rd("tagOf");
            out.push_str(&format!("    let tag = {tag}(v)\n"));
            for v in vs.iter().filter(|v| v.payload.is_empty()) {
                out.push_str(&format!(
                    "    if tag == \"{0}\" {{\n        out.push({0})\n        return out\n    }}\n",
                    v.name
                ));
            }
        }
        if vs.iter().any(|v| !v.payload.is_empty()) {
            let key_of = self.rd("keyOf");
            out.push_str(&format!("    let key = {key_of}(v)\n"));
            for v in vs.iter().filter(|v| !v.payload.is_empty()) {
                let arm = self.payload_arm(&v.name, &v.payload)?;
                out.push_str(&format!(
                    "    if key == \"{}\" {{\n{arm}        return out\n    }}\n",
                    v.name
                ));
            }
        }
        let (ptype, kind) = (self.rd("pushType"), self.rd("kindName"));
        out.push_str(&format!(
            "    {ptype}(iss, path, \"{expected}\", {kind}(v))\n    return out\n"
        ));
        Ok(out)
    }

    /// One payload variant's arm, decoding at `path.Tag` and a tuple's members at
    /// `path.Tag[i]`. The arity is checked first: a short array would otherwise
    /// decode `Option` members from `elemAt`'s out-of-range `JNull` as `None`, a
    /// shape nothing encodes.
    fn payload_arm(&mut self, name: &str, payload: &[Type]) -> Result<String, String> {
        let fpath = self.rd("fieldPath");
        if payload.len() == 1 {
            let d = self.decoder(&payload[0])?;
            let val_of = self.rd("valOf");
            return Ok(format!(
                "        let c = {fpath}(path, \"{name}\")\n\
                 \x20       for a0 in {d}({val_of}(v), c, iss) {{\n\
                 \x20           out.push({name}(a0))\n\
                 \x20       }}\n"
            ));
        }
        let (val_of, kind, ptype, items_of, elem, ipath) = (
            self.rd("valOf"),
            self.rd("kindName"),
            self.rd("pushType"),
            self.rd("itemsOf"),
            self.rd("elemAt"),
            self.rd("indexPath"),
        );
        let arity = payload.len();
        let mut out = format!(
            "        let c = {fpath}(path, \"{name}\")\n\
             \x20       let pv = {val_of}(v)\n\
             \x20       let pk = {kind}(pv)\n\
             \x20       if pk != \"array\" {{\n\
             \x20           {ptype}(iss, c, \"array\", pk)\n\
             \x20           return out\n\
             \x20       }}\n\
             \x20       let items = {items_of}(pv)\n\
             \x20       if items.length != {arity} {{\n\
             \x20           {ptype}(iss, c, \"array of length {arity}\", \"array of length \" + items.length.toString())\n\
             \x20           return out\n\
             \x20       }}\n"
        );
        let mut guards = Vec::new();
        let mut binds = Vec::new();
        for (i, p) in payload.iter().enumerate() {
            let d = self.decoder(p)?;
            out.push_str(&format!(
                "        let mut a{i} = {d}({elem}(items, {i}), {ipath}(c, {i}), iss)\n"
            ));
            guards.push(format!("a{i}.length == 1"));
            // `swapRemove`, not `a{i}[0]`: an element read borrows its container,
            // and the copy that makes it a value is waste here.
            binds.push(format!("a{i}.swapRemove(0)"));
        }
        out.push_str(&format!(
            "        if {} {{\n            out.push({name}({}))\n        }}\n",
            guards.join(" && "),
            binds.join(", ")
        ));
        Ok(out)
    }

    /// Parses the generated source and folds every placeholder onto its reserved
    /// name. A failure is a bug in this module, reported with the source.
    fn parse(self) -> Result<(Vec<Function>, Vec<TypeDecl>), String> {
        let tokens = crate::lexer::lex(&self.source).map_err(|d| {
            format!(
                "internal: fromJson decoders do not lex: {}\n{}",
                d.message, self.source
            )
        })?;
        let (program, errors) = crate::parser::parse_accum(tokens);
        if let Some(d) = errors.first() {
            return Err(format!(
                "internal: fromJson decoders do not parse: {}\n{}",
                d.message, self.source
            ));
        }
        let mut program = program;
        let map: HashMap<String, String> = self
            .names
            .iter()
            .map(|ph| {
                let reserved = if ph.starts_with(PD) {
                    format!("{}{}", rd_prefix(), &ph[PD.len()..])
                } else {
                    format!("{}{}", crate::loader::RT_PREFIX, &ph[PH.len()..])
                };
                (ph.clone(), reserved)
            })
            .collect();
        for f in &mut program.functions {
            if let Some(r) = map.get(&f.name) {
                f.name = r.clone();
            }
        }
        crate::loader::rewrite_names(&mut program, &map);
        let mut out = program.functions;
        for mut f in self.preds {
            if let Some(r) = map.get(&f.name) {
                f.name = r.clone();
            }
            out.push(f);
        }
        let mut aliases = self.aliases;
        for a in &mut aliases {
            if let Some(r) = map.get(&a.name) {
                a.name = r.clone();
            }
        }
        Ok((out, aliases))
    }
}

/// Returns `Int<bits>`'s inclusive bounds as source. A negative bound is
/// written `0 - n`, not a negative literal.
fn signed_bounds(bits: u8) -> (String, String) {
    let hi = (1u128 << (bits - 1)) - 1;
    (format!("0 - {}", hi + 1), hi.to_string())
}

fn unsigned_max(bits: u8) -> String {
    if bits >= 64 {
        u64::MAX.to_string()
    } else {
        ((1u128 << bits) - 1).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Field;

    fn gen(ty: &Type) -> Vec<Function> {
        decoders(&[ty.clone()], &HashMap::new())
            .expect("decoders")
            .0
    }

    /// Every decoder returns an array and every entry point `Validation<T>`. An
    /// unspellable type shows up as a header that parses to a `Unit` return.
    #[test]
    fn every_decoder_answers_in_the_zero_or_one_array() {
        for ty in [
            Type::Int,
            Type::IntN {
                bits: 8,
                signed: true,
            },
            Type::IntN {
                bits: 64,
                signed: false,
            },
            Type::Str,
            Type::Bool,
            Type::Float,
            Type::Array(Box::new(Type::Int)),
            Type::option(Type::Str),
            Type::Record(vec![
                Field {
                    name: "n".into(),
                    ty: Type::Int,
                },
                Field {
                    name: "s".into(),
                    ty: Type::option(Type::Str),
                },
            ]),
            Type::Map(Box::new(Type::Str), Box::new(Type::Int)),
            Type::result(Type::Int, Type::Str),
        ] {
            let fns = gen(&ty);
            assert!(!fns.is_empty(), "no decoder for {ty}");
            let top = fns
                .iter()
                .find(|f| f.name == top_name(&ty))
                .unwrap_or_else(|| panic!("no entry point for {ty}"));
            assert_eq!(
                top.ret,
                Type::App("Validation".to_string(), vec![ty.clone()]),
                "entry point for `{ty}` does not return Validation<T>"
            );
            for f in fns.iter().filter(|f| f.name != top.name) {
                assert!(
                    matches!(f.ret, Type::Array(_)),
                    "decoder `{}` for `{ty}` returns {} rather than an Array",
                    f.name,
                    f.ret
                );
            }
        }
    }

    /// The generated source carries the bounds as literals, so a wrong bound is a
    /// wrong decode, not a compile error.
    #[test]
    fn sized_integer_bounds_are_the_types_own() {
        assert_eq!(signed_bounds(8), ("0 - 128".to_string(), "127".to_string()));
        assert_eq!(
            signed_bounds(32),
            ("0 - 2147483648".to_string(), "2147483647".to_string())
        );
        assert_eq!(unsigned_max(8), "255");
        assert_eq!(unsigned_max(32), "4294967295");
        assert_eq!(unsigned_max(64), "18446744073709551615");
    }
}
