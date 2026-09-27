//! The type-directed JSON encode walk, built once for every
//! backend.
//!
//! `toJson(x)` is `encode(x, T)`, which builds a `std/json` `Json` tree from
//! the static type and needs the compiler, then `std/json`'s `emit`, written
//! in Vyrn. This module generates the first half.
//!
//! It generates Vyrn source and parses it, because the parser builds ASTs and
//! the text is printable when something breaks. Source cannot spell the
//! injected module's reserved `$` names, so the text uses `VyrnRt_`
//! placeholders that one rename pass folds onto `json$X`. It emits one
//! function per distinct type, so a self-referential type
//! (`type Node = { kids: Array<Node> }`) recurses through a call, and the
//! value is a parameter, so `toJson(f())` calls `f` once.

use std::collections::HashMap;

use crate::ast::{Function, Type, TypeDecl};
use crate::codec::Wire;

/// The placeholder prefix for a name generated source cannot spell, folded
/// onto [`crate::loader::RT_PREFIX`] after parsing.
const PH: &str = "VyrnRt_";

/// Returns the reserved name of the encoder for `ty`.
///
/// The name is keyed by [`crate::types::struct_key`], the structural identity
/// the codegen symbols read too, because it must be injective: an encoder
/// picked by a colliding name would encode the wrong shape.
pub fn enc_name(ty: &Type) -> String {
    format!(
        "{}e{}",
        crate::loader::RT_PREFIX,
        crate::types::struct_key(ty)
    )
}

/// Returns the placeholder name of an encoder.
fn enc_ph(ty: &Type) -> String {
    format!("{PH}e{}", crate::types::struct_key(ty))
}

/// Returns whether `ty`'s spelling holds a `lazy` the parser refuses: `lazy`
/// is legal only as a named record's field. A `Type::Named` spells as its name,
/// so the walk stops there.
fn unspellable_lazy(ty: &Type) -> bool {
    match ty {
        Type::Lazy(_) => true,
        Type::Record(fs) => fs.iter().any(|f| unspellable_lazy(&f.ty)),
        Type::Array(t) | Type::ArrayN(t, _) => unspellable_lazy(t),
        Type::Map(a, b) => unspellable_lazy(a) || unspellable_lazy(b),
        Type::Enum(vs) => vs.iter().any(|v| v.payload.iter().any(unspellable_lazy)),
        _ => false,
    }
}

/// Spells a type as generated source, with the injected module's `$` names
/// folded onto placeholders.
fn spell(ty: &Type) -> String {
    ty.to_string().replace(crate::loader::RT_PREFIX, PH)
}

/// Returns the reserved name of the `toJson` wrapper for `ty`; `toJson(x)` at
/// static type `ty` calls it ([`crate::loader::routed_callee`]).
///
/// The wrapper binds the tree, so block exit releases it. The inline form
/// `json$emit(json$e<key>(arg))` made the tree an argument temporary, whose
/// release the drains refuse, and leaked it.
pub fn wrap_name(ty: &Type) -> String {
    format!(
        "{}w{}",
        crate::loader::RT_PREFIX,
        crate::types::struct_key(ty)
    )
}

/// Returns the placeholder name of a wrapper.
fn wrap_ph(ty: &Type) -> String {
    format!("{PH}w{}", crate::types::struct_key(ty))
}

/// Generates the encoders for `tys` and everything they reach, parsed and
/// ready to append to a linked `Program`, where every pass treats them as
/// ordinary functions.
///
/// A type the walk cannot encode is skipped: `toJson` refuses it at the call
/// site (`crate::codec::encodable`), and a program that never encodes it must
/// not fail.
pub fn encoders(tys: &[Type], types: &HashMap<String, TypeDecl>) -> Result<Vec<Function>, String> {
    let mut w = Walk {
        types,
        done: HashMap::new(),
        source: String::new(),
        names: Vec::new(),
    };
    for ty in tys {
        let Ok(e) = w.encoder(ty) else { continue };
        // The `toJson` wrapper (see `wrap_name`).
        let wph = wrap_ph(ty);
        if w.done.contains_key(&wph) {
            continue;
        }
        w.done.insert(wph.clone(), ty.clone());
        w.names.push(wph.clone());
        let (json, emit) = (w.rt("Json"), w.rt("emit"));
        // Bind the render before returning it: `return emit(t)` reads as `t`
        // moving into the return, which skips the block-exit release.
        w.source.push_str(&format!(
            "fn {wph}(v: {}) -> String {{\n\
             \x20   let t: {json} = {e}(v)\n\
             \x20   let s = {emit}(t)\n\
             \x20   return s\n\
             }}\n",
            spell(ty)
        ));
    }
    if w.source.is_empty() {
        return Ok(Vec::new());
    }
    w.parse()
}

struct Walk<'a> {
    types: &'a HashMap<String, TypeDecl>,
    /// Types emitted or in progress. A type is inserted before its body is built,
    /// which makes a self-referential type terminate. The type is kept beside the
    /// name because [`crate::types::struct_key`] truncates to 64 bits: two types
    /// on one name are a build error, not one shared encoder.
    done: HashMap<String, Type>,
    source: String,
    /// Every placeholder the source mentions, for the rename map.
    names: Vec<String>,
}

impl Walk<'_> {
    /// Ensures an encoder for `ty` exists and returns its placeholder name.
    fn encoder(&mut self, ty: &Type) -> Result<String, String> {
        // The value is a parameter, so `ty` needs a source spelling. An anonymous
        // enum has none (declared enums arrive as `Type::Named`; `Display` spells
        // `Option` and `Result`), and neither does a bare or anonymously nested
        // `lazy T`. Refuse both here instead of handing the parser text it rejects.
        if matches!(ty, Type::Enum(_)) && !crate::types::is_sum_alias(ty) {
            return Err("toJson: cannot encode an anonymous enum".to_string());
        }
        if unspellable_lazy(ty) {
            return Err("toJson: cannot encode a bare `lazy` type".to_string());
        }
        let ph = enc_ph(ty);
        if let Some(prev) = self.done.get(&ph) {
            if prev != ty {
                return Err(format!(
                    "internal: {prev} and {ty} share the encoder name `{ph}`"
                ));
            }
            return Ok(ph);
        }
        // Reserve the name before the body, so a recursive type gets the name.
        self.done.insert(ph.clone(), ty.clone());
        self.names.push(ph.clone());
        let body = self.body(ty)?;
        // Through `rt`, not inline: an unregistered placeholder is never renamed,
        // and the return type would be an unknown named type.
        let json = self.rt("Json");
        self.source.push_str(&format!(
            "fn {ph}(v: {}) -> {json} {{\n{body}}}\n",
            spell(ty)
        ));
        Ok(ph)
    }

    fn rt(&mut self, name: &str) -> String {
        let ph = format!("{PH}{name}");
        if !self.names.contains(&ph) {
            self.names.push(ph.clone());
        }
        ph
    }

    /// Returns the body of `ty`'s encoder: the canonical encoding of the
    /// wire form [`crate::codec::wire`] decides, spelled as source.
    fn body(&mut self, ty: &Type) -> Result<String, String> {
        match crate::codec::wire(ty, self.types, false)
            .map_err(|t| format!("toJson: cannot encode {t}"))?
        {
            Wire::Int | Wire::IntN { .. } | Wire::Float | Wire::Float32 => {
                let n = self.rt("JNum");
                Ok(format!("    return {n}(v.toString())\n"))
            }
            Wire::Bool => {
                let n = self.rt("JBool");
                Ok(format!("    return {n}(v)\n"))
            }
            Wire::Str => {
                let n = self.rt("JStr");
                // Copy: `v` is a `read` parameter and the `Json` outlives the call, so
                // sharing the buffer would leak the argument and the result.
                Ok(format!("    return {n}(v.copy())\n"))
            }
            Wire::Record(fields) => {
                let (obj, fld) = (self.rt("JObj"), self.rt("JsonField"));
                let mut out = format!("    let mut fs: Array<{fld}> = []\n");
                for f in &fields {
                    // A `lazy T` field encodes as `T`; `v.{name}` forces it, so every deferred
                    // field is computed whether or not a selection asked for it.
                    let fty = crate::types::forced(&f.ty);
                    // A `None` field is omitted. `push` returns the array, so a
                    // `match` yields the pushed or the untouched array.
                    if let Ok(Wire::Option(inner)) = crate::codec::wire(&fty, self.types, false) {
                        let e = self.encoder(&inner)?;
                        out.push_str(&format!(
                            "    fs = match v.{0} {{ Some(x) => fs.push({fld} {{ key: \"{0}\", value: {e}(x) }}), None => fs }}\n",
                            f.name
                        ));
                    } else {
                        let e = self.encoder(&fty)?;
                        out.push_str(&format!(
                            "    fs.push({fld} {{ key: \"{0}\", value: {e}(v.{0}) }})\n",
                            f.name
                        ));
                    }
                }
                out.push_str(&format!("    return {obj}(fs)\n"));
                Ok(out)
            }
            // A bare `Option`: `Some` encodes the payload, `None` is `null`.
            Wire::Option(inner) => {
                let e = self.encoder(&inner)?;
                let null = self.rt("JNull");
                Ok(format!(
                    "    return match v {{ Some(x) => {e}(x), None => {null} }}\n"
                ))
            }
            Wire::Array(inner) | Wire::FixedArray(inner, _) => {
                let e = self.encoder(&inner)?;
                let (arr, json) = (self.rt("JArr"), self.rt("Json"));
                Ok(format!(
                    "    let mut out: Array<{json}> = []\n    for it in v {{ out.push({e}(it)) }}\n    return {arr}(out)\n"
                ))
            }
            // A `Map<String, V>` is an object: keys in insertion order.
            Wire::Map(v) => {
                let e = self.encoder(&v)?;
                let (obj, fld) = (self.rt("JObj"), self.rt("JsonField"));
                Ok(format!(
                    "    let mut fs: Array<{fld}> = []\n    for k in v.keys() {{\n        fs = match v[k] {{ Some(x) => fs.push({fld} {{ key: k, value: {e}(x) }}), None => fs }}\n    }}\n    return {obj}(fs)\n"
                ))
            }
            // A `Map<Int64, V>`: keys are the decimal texts `toString`
            // writes, which `dIntKey` reads back.
            Wire::MapI(v) => {
                let e = self.encoder(&v)?;
                let (obj, fld) = (self.rt("JObj"), self.rt("JsonField"));
                Ok(format!(
                    "    let mut fs: Array<{fld}> = []\n    for k in v.keys() {{\n        fs = match v[k] {{ Some(x) => fs.push({fld} {{ key: k.toString(), value: {e}(x) }}), None => fs }}\n    }}\n    return {obj}(fs)\n"
                ))
            }
            // `Result<T, E>` arrives as its two-variant enum.
            Wire::Enum(vs) => {
                let mut arms = String::new();
                for var in &vs {
                    let binds: Vec<String> =
                        (0..var.payload.len()).map(|i| format!("p{i}")).collect();
                    let pat = if binds.is_empty() {
                        var.name.clone()
                    } else {
                        format!("{}({})", var.name, binds.join(", "))
                    };
                    let value = self.wire_variant(&var.name, &var.payload, &binds)?;
                    arms.push_str(&format!("        {pat} => {value},\n"));
                }
                Ok(format!("    return match v {{\n{arms}    }}\n"))
            }
        }
    }

    /// One variant in wire form: nullary is a bare string, one payload
    /// `{"Tag":<v>}`, several `{"Tag":[..]}`.
    fn wire_variant(
        &mut self,
        name: &str,
        payload: &[Type],
        binds: &[String],
    ) -> Result<String, String> {
        if payload.is_empty() {
            let s = self.rt("JStr");
            return Ok(format!("{s}(\"{name}\")"));
        }
        let (obj, fld) = (self.rt("JObj"), self.rt("JsonField"));
        if payload.len() == 1 {
            let e = self.encoder(&payload[0])?;
            return Ok(format!(
                "{obj}([{fld} {{ key: \"{name}\", value: {e}({}) }}])",
                binds[0]
            ));
        }
        let arr = self.rt("JArr");
        let mut items = Vec::new();
        for (i, p) in payload.iter().enumerate() {
            let e = self.encoder(p)?;
            items.push(format!("{e}({})", binds[i]));
        }
        Ok(format!(
            "{obj}([{fld} {{ key: \"{name}\", value: {arr}([{}]) }}])",
            items.join(", ")
        ))
    }

    /// Parses the generated source and folds every placeholder onto its reserved
    /// name. A failure is a bug in this module, reported with the source.
    fn parse(self) -> Result<Vec<Function>, String> {
        let tokens = crate::lexer::lex(&self.source).map_err(|d| {
            format!(
                "internal: toJson encoders do not lex: {}\n{}",
                d.message, self.source
            )
        })?;
        let (program, errors) = crate::parser::parse_accum(tokens);
        if let Some(d) = errors.first() {
            return Err(format!(
                "internal: toJson encoders do not parse: {}\n{}",
                d.message, self.source
            ));
        }
        let mut program = program;
        let map: HashMap<String, String> = self
            .names
            .iter()
            .map(|ph| {
                (
                    ph.clone(),
                    format!("{}{}", crate::loader::RT_PREFIX, &ph[PH.len()..]),
                )
            })
            .collect();
        for f in &mut program.functions {
            if let Some(r) = map.get(&f.name) {
                f.name = r.clone();
            }
        }
        crate::loader::rewrite_names(&mut program, &map);
        Ok(program.functions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Field;

    fn gen(ty: &Type) -> Vec<Function> {
        encoders(&[ty.clone()], &HashMap::new()).expect("encoders")
    }

    /// Every encoder returns the tree type; an unspellable parameter type shows up
    /// as a header that parses to a `Unit` return.
    #[test]
    fn every_encoder_returns_the_tree_type() {
        for ty in [
            Type::Int,
            Type::Str,
            Type::Bool,
            Type::Array(Box::new(Type::Int)),
            Type::option(Type::Str),
            Type::Record(vec![
                Field {
                    name: "n".into(),
                    ty: Type::Int,
                },
                Field {
                    name: "s".into(),
                    ty: Type::Str,
                },
            ]),
            Type::Map(Box::new(Type::Str), Box::new(Type::Int)),
            Type::result(Type::Int, Type::Str),
        ] {
            let fns = gen(&ty);
            assert!(!fns.is_empty(), "no encoder for {ty}");
            // One wrapper per root returns the rendered `String`; every other function
            // is an encoder and returns the tree.
            let wrap = wrap_name(&ty);
            let mut wraps = 0;
            for f in &fns {
                if f.name == wrap {
                    wraps += 1;
                    assert_eq!(f.ret, Type::Str, "the wrapper returns the render");
                    continue;
                }
                assert_eq!(
                    f.ret,
                    Type::Named(format!("{}Json", crate::loader::RT_PREFIX)),
                    "encoder `{}` for `{ty}` does not return the tree type",
                    f.name
                );
            }
            assert_eq!(wraps, 1, "exactly one `toJson` wrapper for {ty}");
        }
    }
}
