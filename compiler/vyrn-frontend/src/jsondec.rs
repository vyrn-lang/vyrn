//! The type-directed JSON decode walk, as ONE shared AST builder (RFC-0078 M3).
//!
//! `fromJson(T, s)` is two halves, and only the first needs the compiler:
//!
//! ```text
//! read(s)          -> a `Json` tree                  [does not — std/jsonread]
//! decode(tree, T)  -> a value of T, plus `Issue`s    [needs the compiler]
//! ```
//!
//! This module is the second half and it exists once: it turns a decode TARGET
//! type into Vyrn functions that walk a `std/json` `Json` value, so the
//! interpreter, the textual emitter and both wasm backends compile the same tree
//! instead of holding three decoders (Rust, C, and whatever the direct backend
//! would have had to grow). It is [`crate::jsonenc`] run backwards, and it shares
//! that module's two mechanisms unchanged: generated SOURCE handed to the parser,
//! and one function per distinct type so a self-referential type terminates.
//!
//! # Why the result of a decoder is `Array<T>` and not `Option<T>`
//!
//! RFC-0018 decode accumulates: it does not stop at the first problem, so a
//! decoder must record a failure and keep walking, and a composite must construct
//! only once every part succeeded — a refined type cannot hold a value that failed
//! its own predicate, and there is no zeroed slot a Vyrn program can spell.
//!
//! RFC-0078 M3 predicted `Option<T>` for that shape. It cannot be: a bare
//! `Option<U>` IS a decode target (`Array<Option<Int64>>` decodes today) and
//! `Option<Option<U>>` has no wire form — a double `null` has two readings, so
//! [`crate::codec::wire`] declines it. That refusal is the codec's own and
//! stands; the checker stopped refusing the TYPE at RFC-0126 §8. So a decoder
//! returns an array of **zero or one** element, which is one convention for
//! every `T` including `T = Option<U>`, and which reads well at the use site:
//! `for x in dec(..) { .. }` runs exactly when a value was produced.
//!
//! # Where a predicate comes from
//!
//! Not from here. A refined type's `where` clause is lowered in exactly one place
//! per engine, and the STRUCTURE it binds is [`crate::types::predicate_binds`] —
//! shared with the trap path so the two cannot disagree about what `value` means.
//! This module synthesizes a `Bool`-returning function whose body is the
//! predicate's own AST and whose parameters are that same structure, so the decode
//! path runs the identical expression the trap path does.

use std::collections::HashMap;

use crate::ast::{Block, Capability, Function, Param, Stmt, Type, TypeDecl};
use crate::codec::Wire;

/// The placeholder prefix for a name in the injected `std/json` (the tree).
const PH: &str = "VyrnRt_";
/// The placeholder prefix for a name in the injected `std/jsondec` (the untyped
/// half). Two prefixes rather than one because two runtime modules are involved
/// and each is renamed to its own reserved spelling.
const PD: &str = "VyrnRd_";

/// The reserved prefix `std/jsondec`'s declarations are renamed to.
fn rd_prefix() -> &'static str {
    "jsondec$"
}

/// The type's structural identity, unlike a readable mangle: RFC-0077 M2e found
/// two instantiations colliding on one symbol, and a decoder picked by a
/// colliding name would decode the wrong shape rather than fail to build. The
/// identity is [`crate::types::struct_key`] — one definition, read here, by
/// [`crate::jsonenc`] and by the codegen symbols.
fn type_key(ty: &Type) -> String {
    crate::types::struct_key(ty)
}

/// The top-level `String -> Validation<T>` entry point for a decode target, by
/// its reserved (unspellable) name.
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

/// A type as generated source: `Display` is the source spelling, with the
/// injected modules' `$` names folded onto placeholders.
fn spell(ty: &Type) -> String {
    ty.to_string()
        .replace(crate::loader::RT_PREFIX, PH)
        .replace(rd_prefix(), PD)
}

/// What `fromJson(T, s)` becomes: a call to `T`'s synthesized entry point. Every
/// engine calls this at its own `fromJson` arm, so the walk has one definition and
/// no engine holds a JSON decoder.
pub fn decode_expr(target: &Type, src: crate::ast::Expr, line: usize) -> crate::ast::Expr {
    crate::ast::Expr::Call {
        name: top_name(target),
        args: vec![src],
        line,
    }
}

/// Generate the decoders for `tys` (each a `fromJson` target) and everything
/// reachable from them, ready to be appended to a linked `Program`.
///
/// A type the walk cannot decode is SKIPPED rather than fatal, exactly as
/// [`crate::jsonenc::encoders`] skips one it cannot encode: `fromJson` rejects
/// those at the call site with its own diagnostic (`crate::codec::decodable`), and
/// a type that merely sits in the collected set must not fail a program that never
/// decodes it.
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
    if std::env::var("VYRN_DEC_DUMP").is_ok() {
        eprintln!(
            "=== generated decoders ===
{}",
            w.source
        );
    }
    w.parse()
}

struct Walk<'a> {
    types: &'a HashMap<String, TypeDecl>,
    /// Names already emitted (or in progress — inserted BEFORE the body is built,
    /// which is what makes a self-referential type terminate).
    ///
    /// The TYPE is kept beside its name, not a `()`, and that is the whole answer
    /// to the 64 bits [`crate::types::struct_key`] truncates to: two distinct
    /// types on one name would otherwise mean the second decoder is silently
    /// skipped and both call sites decode through the first one's shape. Holding
    /// the type makes that a build error naming both.
    done: HashMap<String, Type>,
    source: String,
    /// Every placeholder name the source mentions, for the rename map.
    names: Vec<String>,
    /// Predicate functions, built as AST rather than as source: their body is the
    /// `where` clause's own expression, which has no source spelling to print.
    preds: Vec<Function>,
    /// Named aliases for ANONYMOUS record types. Vyrn has no anonymous record
    /// LITERAL — `{ c: 1 }` is not an expression, only `T { c: 1 }` is — so a
    /// nested inline record type (`type A = { b: { c: Int64 } }`, which decodes
    /// today) has no spelling a decoder could construct. A type position accepts
    /// the anonymous form perfectly well, so what is synthesized is one transparent
    /// alias per shape, used for the literal and nothing else.
    aliases: Vec<TypeDecl>,
}

impl Walk<'_> {
    /// Claim `ph` for `ty`, or say it is already taken. `Err` is the collision
    /// [`crate::types::struct_key`]'s truncation leaves possible: two distinct
    /// types on one name, which without this check emits one body and decodes
    /// both call sites through it.
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

    /// Ensure the top-level entry point for a decode target exists.
    fn top(&mut self, ty: &Type) -> Result<String, String> {
        let ph = top_ph(ty);
        if !self.reserve(&ph, ty)? {
            return Ok(ph);
        }
        let d = self.decoder(ty)?;
        let (rd, t) = (self.rd("readDoc"), spell(ty));
        // The doc is walked by CONSUME (each root's tree is released as it is
        // decoded, and the buffer with the loop), and the issue returns TAKE
        // the accumulator — the spellings whose releases every engine already
        // proves (exit-residue round thirteen: the plain walk and the bare
        // returns left the doc buffer and the decoded snapshot behind, one
        // pair per `fromJson`).
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

    /// Ensure a decoder for `ty` exists and return its placeholder call name.
    fn decoder(&mut self, ty: &Type) -> Result<String, String> {
        if matches!(ty, Type::Enum(_)) {
            // An anonymous enum has no source spelling (`Display` renders
            // `enum { A | B }`), so it cannot be a return type. Every enum a
            // program declares arrives as `Type::Named`.
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

    /// Register a placeholder for a `std/json` name and return its spelling.
    fn rt(&mut self, name: &str) -> String {
        let ph = format!("{PH}{name}");
        if !self.names.contains(&ph) {
            self.names.push(ph.clone());
        }
        ph
    }

    /// The same for a `std/jsondec` name.
    fn rd(&mut self, name: &str) -> String {
        let ph = format!("{PD}{name}");
        if !self.names.contains(&ph) {
            self.names.push(ph.clone());
        }
        ph
    }

    /// The body of `ty`'s decoder, as statements.
    ///
    /// WHAT a type is on the wire is [`crate::codec::wire`]'s answer, not this
    /// module's — the same answer the checker's gate, the schema emitter and
    /// [`crate::jsonenc`] read. This function only spells it as source, backwards.
    fn body(&mut self, ty: &Type) -> Result<String, String> {
        // A NAMED type is not resolved away first: its `where` clause is the whole
        // reason decode has an accumulating shape, so a refinement decodes its
        // base and then guards.
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
                // The literal needs a NAME. A named target has one; an anonymous
                // record gets a synthesized alias, because `{ c: 1 }` is not an
                // expression in Vyrn.
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
            // `Result<T, E>` arrives as the two-variant enum it is on the wire,
            // so external tagging is read back in one place — and its
            // ``one of `Ok`, `Err``` falls out of listing those two variants
            // rather than being a second spelling of the same sentence.
            Wire::Enum(vs) => {
                let expected = crate::codec::enum_expected(&vs);
                self.variants_body(&t, &vs, &expected)
            }
            // Unreachable: `wire` in the decode direction has no fixed array —
            // its length is not known until the data arrives, so it left as an
            // `Err` above.
            Wire::FixedArray(..) => Err(format!("fromJson: cannot decode {ty}")),
        }
    }

    /// A scalar whose helper already answers in the target type.
    fn scalar(&mut self, helper: &str, extra: &str) -> String {
        let h = self.rd(helper);
        format!("    return {h}(v, path, iss{extra})\n")
    }

    /// A sized integer: the helper answers in `Int64`/`UInt64` and the narrowing
    /// conversion is applied to the one value it produced. In range by
    /// construction — the helper refused everything else — so the conversion
    /// cannot wrap.
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
    /// it holds. Pushing into `Array<Named>` is what performs the construction —
    /// an array element store coerces and validates, which is the same boundary
    /// `Age(n)` goes through and works for a record base too, where there is no
    /// `Name(value)` constructor form.
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

    /// The `Bool`-returning function whose body is the `where` clause itself.
    /// Built as AST rather than printed: a predicate is an `Expr` with no source
    /// spelling, and its parameters are [`crate::types::predicate_binds`] — the
    /// same structure the trap path binds, which is what keeps the two from
    /// drifting.
    ///
    /// No collision check, unlike [`Walk::reserve`]'s two callers, and it needs
    /// none: a predicate's name is keyed on the same type its decoder is, so two
    /// distinct types colliding here have already collided on `dec_ph` and the
    /// build stopped there.
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
                    name,
                    capability: Capability::Read,
                    ty,
                })
                .collect(),
            ret: Type::Bool,
            body: Block {
                stmts: vec![Stmt::Return {
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

    /// A record: every field decoded into its own staging array, then constructed
    /// once every REQUIRED field produced a value. An `Option<T>` field is inlined
    /// rather than given a decoder — absent OR `null` is `None` and neither is a
    /// failure (RFC-0018), and a failing inner decode leaves `None` with its issue
    /// already recorded.
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
            if let Type::Option(inner) = crate::types::resolve(&f.ty, self.types) {
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
                // The decoded value is TAKEN out of its one-element carrier —
                // see the `swapRemove` note in `variant_payload`.
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

    /// An array: every element decoded at `path[i]`, and only the ones that
    /// produced a value collected. The array itself always succeeds when the node
    /// IS an array — a bad element is an issue, not a missing array.
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

    /// A `Map<String, V>` decodes any JSON object (RFC-0028): document order
    /// becomes insertion order, each value is decoded at `path.<key>`, and a
    /// repeated key keeps the first — which `std/jsonread` makes unreachable, since
    /// it rejects duplicates outright.
    ///
    /// **The key is COPIED into the map.** A map takes its key (RFC-0092 M5 —
    /// both compiling backends write the key pointer in and copy nothing), and
    /// `f` is an element of the snapshot `fieldsOf(v)` returns. While `Json` had
    /// no release rule that snapshot leaked and the sharing was invisible; since
    /// RFC-0096 M2 it is released, so the map and the snapshot both owned one
    /// buffer and the map's key came back freed — `fromJson(Map<String, Int64>,
    /// "{"apple":3,"pear":5}")` summed to 4 rather than 8 on wasm.
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

    /// The `Map<Int64, V>` twin (RFC-0117 M3): each field's KEY reads back
    /// through `dIntKey` — canonical decimal text only, an Issue otherwise —
    /// and a duplicate key keeps its first value, exactly as the String-keyed
    /// body does.
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

    /// A payload enum or `Result` in RFC-0024 wire form: a bare string is a
    /// nullary variant, a ONE-key object `{"Tag":..}` names a payload variant
    /// (single payload direct, tuple payload as an array). Exactly one wire form
    /// per value — a payload variant spelled as a bare string, a nullary one as an
    /// object, an unknown key, or an object with zero or two or more members all
    /// fail with the locked expected-one-of `json.type` Issue.
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

    /// One payload variant's arm: the payload path is `path.Tag`, and a tuple
    /// payload's members are `path.Tag[i]` against the wire array's elements.
    /// The encoder always writes exactly one element per member, so the arity
    /// is checked up front — without the check, a short array would decode
    /// every-`Option` members against `elemAt`'s out-of-range `JNull` and
    /// succeed as all-`None`, a shape nothing can encode.
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
            // `swapRemove` and not `a{i}[0]`: an element read is a borrow of its
            // container (RFC-0092), and the copy that would make it a value is
            // pure waste here — the one-element array dies on the next line.
            binds.push(format!("a{i}.swapRemove(0)"));
        }
        out.push_str(&format!(
            "        if {} {{\n            out.push({name}({}))\n        }}\n",
            guards.join(" && "),
            binds.join(", ")
        ));
        Ok(out)
    }

    /// Lex + parse the generated source, then fold every placeholder onto its
    /// reserved spelling. A failure here is a bug in this module, so it reports
    /// with the source attached rather than as a user diagnostic.
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

/// `Int<bits>`'s inclusive bounds, spelled so the generated source parses: a
/// negative literal is written `0 - n`, because `-128` as a token would be a
/// unary minus the emitters do not need to see here.
fn signed_bounds(bits: u8) -> (String, String) {
    let hi = (1u128 << (bits - 1)) - 1;
    (format!("0 - {}", hi + 1), hi.to_string())
}

/// `UInt<bits>`'s maximum.
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

    /// Every decoder answers in the 0-or-1 array convention, and every entry point
    /// in `Validation<T>`. A `Unit` return means the header did not parse as
    /// intended — which is exactly how an unspellable type shows up, silently, as a
    /// `void`-returning function whose body returns a value (RFC-0078 M2b's first
    /// bug).
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
            Type::Option(Box::new(Type::Str)),
            Type::Record(vec![
                Field {
                    name: "n".into(),
                    ty: Type::Int,
                },
                Field {
                    name: "s".into(),
                    ty: Type::Option(Box::new(Type::Str)),
                },
            ]),
            Type::Map(Box::new(Type::Str), Box::new(Type::Int)),
            Type::Result(Box::new(Type::Int), Box::new(Type::Str)),
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

    /// A sized integer's bounds are the type's, not a widened approximation: the
    /// generated source carries them as literals, so a wrong bound is a wrong
    /// decode rather than a compile error.
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
