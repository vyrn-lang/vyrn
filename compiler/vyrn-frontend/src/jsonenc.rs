//! The type-directed JSON encode walk, as ONE shared AST builder (RFC-0078 M2b).
//!
//! `toJson(x)` is two halves. Only the first needs the compiler:
//!
//! ```text
//! encode(x, T)  -> build a JSON tree from the STATIC type   [needs the compiler]
//! emit(tree)    -> serialize the tree to text               [does not — it is Vyrn]
//! ```
//!
//! The second half already exists as `std/json`'s `emit`, written in Vyrn and
//! parity-tested three ways. This module is the first half, and it exists once: it
//! turns a value of a known static type into an expression that builds a
//! `std/json` `Json` value, so the interpreter, the textual emitter and BOTH wasm
//! backends compile the same tree instead of holding three encoders (Rust, C, and
//! whatever the direct backend would have had to grow).
//!
//! # Why it generates source text
//!
//! Because the parser is a perfectly good AST builder and hand-writing `Expr`
//! trees is a hundred lines of noise per shape. The walk emits ordinary Vyrn —
//! readable, and printable when something goes wrong — then lexes and parses it.
//! The one thing it cannot spell is the injected module's reserved names
//! (`json$emit` has a `$` in it, which is exactly why no source can name it), so
//! the text uses `VyrnRt_`-prefixed placeholders and one rename pass folds
//! `VyrnRt_X` into `json$X` afterwards. That is a single rule, and it also covers
//! encoding a `Json` value itself.
//!
//! # Why per-type FUNCTIONS and not an inline walk
//!
//! A self-referential type — `type Node = { kids: Array<Node> }`, which `toJson`
//! encodes today — makes an inline AST walk non-terminating. So the walk emits one
//! function per distinct type, memoized on the type, and recursion becomes a call.
//! That is the AST analogue of the IR's `__vyrn_enc_{n}`, which exists for exactly
//! the same reason. It also fixes evaluation order for free: the value is a
//! parameter, so `toJson(f())` calls `f` once.

use std::collections::HashMap;

use crate::ast::{Function, Type, TypeDecl};
use crate::codec::Wire;

/// The placeholder prefix the generated source uses for a name it cannot spell.
/// Folded into [`crate::loader::RT_PREFIX`] after parsing.
const PH: &str = "VyrnRt_";

/// The encoder function for `ty`, by its reserved (unspellable) name.
///
/// Keyed by the type's structural identity rather than a readable mangle, because
/// this name has to be INJECTIVE: `mangle_ty` is not (RFC-0077 M2e found two
/// instantiations colliding on one symbol and the driver silently skipping the
/// second), and an encoder picked by a colliding name would encode the wrong
/// shape rather than fail to build. The identity itself — why the derived `Debug`
/// form serves as the structural serialization, and what pins it — is
/// [`crate::types::struct_key`], which the codegen symbols read too.
pub fn enc_name(ty: &Type) -> String {
    format!(
        "{}e{}",
        crate::loader::RT_PREFIX,
        crate::types::struct_key(ty)
    )
}

/// The placeholder spelling of an encoder, for use inside generated source.
fn enc_ph(ty: &Type) -> String {
    format!("{PH}e{}", crate::types::struct_key(ty))
}

/// Whether `ty`'s `Display` spelling contains a `lazy` the parser will refuse.
///
/// `lazy` is legal in exactly one position — a field of a NAMED record — so a
/// bare one, or one inside an anonymous record, cannot be written as a parameter
/// type. A `Type::Named` stops the walk: its spelling is the name, and whatever
/// `lazy` its declaration holds is not printed.
fn unspellable_lazy(ty: &Type) -> bool {
    match ty {
        Type::Lazy(_) => true,
        Type::Record(fs) => fs.iter().any(|f| unspellable_lazy(&f.ty)),
        Type::Option(t) | Type::Array(t) | Type::ArrayN(t, _) => unspellable_lazy(t),
        Type::Result(a, b) | Type::Map(a, b) => unspellable_lazy(a) || unspellable_lazy(b),
        _ => false,
    }
}

/// A type as generated source. `Display` is the user-facing spelling — which is
/// exactly the source spelling — with the injected module's `$` names folded onto
/// placeholders.
fn spell(ty: &Type) -> String {
    ty.to_string().replace(crate::loader::RT_PREFIX, PH)
}

/// What `toJson(arg)` becomes when the argument's static type is `ty`:
/// `json$emit(json$e<key>(arg))`. Every engine calls this at the point where it
/// already knows `ty`, so the walk has one definition and no engine has a JSON
/// encoder of its own.
pub fn encode_expr(arg: crate::ast::Expr, ty: &Type, line: usize) -> crate::ast::Expr {
    crate::ast::Expr::Call {
        name: format!("{}emit", crate::loader::RT_PREFIX),
        args: vec![crate::ast::Expr::Call {
            name: enc_name(ty),
            args: vec![arg],
            line,
        }],
        line,
    }
}

/// Generate the encoder functions for `tys` and everything reachable from them.
///
/// Returns the parsed functions, ready to be appended to a linked `Program`: they
/// are ordinary Vyrn functions and every pass downstream — checker, movecheck,
/// ownership, all three emitters — treats them as such. A type the walk cannot
/// encode is SKIPPED rather than fatal: `toJson` rejects those at the call site
/// with its own diagnostic (`crate::codec::encodable`), and a type that merely
/// sits in the collected set unencodable must not fail a program that never
/// encodes it.
pub fn encoders(tys: &[Type], types: &HashMap<String, TypeDecl>) -> Result<Vec<Function>, String> {
    let mut w = Walk {
        types,
        done: HashMap::new(),
        source: String::new(),
        names: Vec::new(),
    };
    for ty in tys {
        // Best-effort per root: an unencodable type in the set is the call site's
        // problem, not this pass's.
        let _ = w.encoder(ty);
    }
    if w.source.is_empty() {
        return Ok(Vec::new());
    }
    w.parse()
}

struct Walk<'a> {
    types: &'a HashMap<String, TypeDecl>,
    /// Types already emitted (or in progress — inserted BEFORE the body is built,
    /// which is what makes a self-referential type terminate).
    ///
    /// The TYPE is kept beside its name, not a `()`, and that is the whole
    /// answer to the 64 bits [`crate::types::struct_key`] truncates to: two
    /// distinct types on one name would otherwise mean the second encoder is
    /// silently skipped and both call sites encode through the first one's shape.
    /// Holding the type makes that a build error naming both.
    done: HashMap<String, Type>,
    source: String,
    /// Every placeholder name the source mentions, for the rename map.
    names: Vec<String>,
}

impl Walk<'_> {
    /// Ensure an encoder for `ty` exists and return its placeholder call name.
    fn encoder(&mut self, ty: &Type) -> Result<String, String> {
        // Every encoder takes its value as a PARAMETER, so `ty` has to have a
        // source spelling the parser accepts. Two shapes do not, and both are
        // rejected here rather than handed to the parser as text it will refuse
        // with an internal error:
        //
        // - an ANONYMOUS enum (`Display` renders `enum { A | B }`). Named enums —
        //   every enum a program declares — arrive as `Type::Named` and are fine;
        // - a bare or anonymously-nested `lazy T`, which the parser accepts only
        //   as a NAMED record's field. A named record's encoder never asks for
        //   one (the record arm forces the field first), so this is the guard for
        //   a type no source can write.
        if matches!(ty, Type::Enum(_)) {
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
        // Reserved BEFORE the body: a recursive type reaches itself here and gets
        // the name rather than another expansion.
        self.done.insert(ph.clone(), ty.clone());
        self.names.push(ph.clone());
        let body = self.body(ty)?;
        // Through `rt`, not spelled inline: a placeholder that is not REGISTERED
        // never reaches the rename map, and an unrenamed return type is an unknown
        // named type that lowers to `void` — a function whose body returns a value
        // and whose signature says it does not, which clang rejects at the caller.
        // (It went unnoticed at first because the `Array` body happens to register
        // the same name, so any program encoding an array looked fine.)
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

    /// The body of `ty`'s encoder, as statements. Mirrors RFC-0018's canonical
    /// encoding exactly.
    ///
    /// WHAT a type is on the wire is [`crate::codec::wire`]'s answer, not this
    /// module's — the same answer the checker's gate and the schema emitter read.
    /// This function only spells it as source.
    fn body(&mut self, ty: &Type) -> Result<String, String> {
        // A named type routes through its own declaration, so recursion breaks at
        // the name; a refinement (`type Port = Int64 where ..`) takes its base.
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
                // `.copy()`, because `v` is a `read` parameter and the `Json`
                // built here outlives the call. Handing the buffer over would
                // make the result name storage the CALLER still owns — the
                // census's finding 2, which leaks the argument and the result
                // both. `toJson(x)` may not take `x`, so the copy is the fix the
                // menu names, and the encoded value owns its own bytes.
                Ok(format!("    return {n}(v.copy())\n"))
            }
            Wire::Record(fields) => {
                let (obj, fld) = (self.rt("JObj"), self.rt("JsonField"));
                let mut out = format!("    let mut fs: Array<{fld}> = []\n");
                for f in &fields {
                    // A `lazy T` field encodes as a `T`, and `v.{name}` in the
                    // generated source is the read that FORCES it (RFC-0085
                    // M4a). Forcing here is correct and it is also exactly why
                    // M4b needs a selective encoder: this encoder computes every
                    // deferred field whether or not a GraphQL selection asked
                    // for one.
                    let fty = crate::types::forced(&f.ty);
                    // A `None` record field is OMITTED entirely (RFC-0018). Spelled
                    // as a `match` whose arms are the pushed array or the untouched
                    // one, rather than the `if let` that reads more naturally,
                    // because `push` RETURNS the array and `if let` is one of
                    // RFC-0077's own unlowered rows — the natural spelling would put
                    // every example with an `Option` field behind it.
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
            // A `Map<String, V>` encodes as an object: keys in insertion order,
            // values through V's codec (RFC-0028).
            Wire::Map(v) => {
                let e = self.encoder(&v)?;
                let (obj, fld) = (self.rt("JObj"), self.rt("JsonField"));
                Ok(format!(
                    "    let mut fs: Array<{fld}> = []\n    for k in v.keys() {{\n        fs = match v[k] {{ Some(x) => fs.push({fld} {{ key: k, value: {e}(x) }}), None => fs }}\n    }}\n    return {obj}(fs)\n"
                ))
            }
            // `Result<T, E>` arrives here as the two-variant enum it is on the
            // wire, so external tagging is written once.
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

    /// One enum/`Result` variant in RFC-0024 wire form: a nullary variant is a
    /// bare string, one payload is `{"Tag":<v>}`, two or more is `{"Tag":[..]}`.
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

    /// Lex + parse the generated source, then fold every `VyrnRt_` placeholder
    /// onto its reserved spelling. A failure here is a bug in this module, so it
    /// reports with the source attached rather than as a user diagnostic.
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

    /// Every encoder returns the tree type. A `Unit` return here means the header
    /// did not parse as intended — which is exactly how an unspellable parameter
    /// type shows up, silently, as a `void`-returning function whose body returns
    /// a value.
    #[test]
    fn every_encoder_returns_the_tree_type() {
        for ty in [
            Type::Int,
            Type::Str,
            Type::Bool,
            Type::Array(Box::new(Type::Int)),
            Type::Option(Box::new(Type::Str)),
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
            Type::Result(Box::new(Type::Int), Box::new(Type::Str)),
        ] {
            let fns = gen(&ty);
            assert!(!fns.is_empty(), "no encoder for {ty}");
            for f in &fns {
                assert_eq!(
                    f.ret,
                    Type::Named(format!("{}Json", crate::loader::RT_PREFIX)),
                    "encoder `{}` for `{ty}` does not return the tree type",
                    f.name
                );
            }
        }
    }
}
