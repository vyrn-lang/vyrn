//! Module reflection for generator imports (RFC-0021).
//!
//! `moduleInterface(path)` parses a module and hands a generator the structured
//! shape of its **exported** surface — this is `schemaOf` generalized from one
//! type to a whole module. The compiler builds a `ModuleInterface` record
//! *literal* (an [`Expr`]) here and the interpreter evaluates it, reusing the
//! ordinary record/array/coercion machinery (exactly the `schemaOf` technique).
//!
//! The shape (all injected in the parser, LSP-filtered by their line-0 origin):
//! ```text
//! ModuleInterface { functions: Array<FnInfo>, types: Array<TypeInfo> }
//! FnInfo   { name: String, params: Array<ParamInfo>, ret: String, retSchema: Schema }
//! ParamInfo{ name: String, spelling: String, schema: Schema }
//! TypeInfo { name: String, source: String, schema: Schema }
//! ```
//! `ret`/`spelling` are the raw type *spellings* (for stub emission); a
//! `TypeInfo.source` is the canonical `type` declaration text (for verbatim
//! re-emission of contract types); the `Schema` values carry the RFC-0009
//! reflection (bounds/pattern/length).

use std::collections::HashMap;

use crate::ast::*;

/// Build the `ModuleInterface` record literal for `program`'s exported surface.
pub fn module_interface_lit(program: &Program) -> Expr {
    let types: HashMap<String, TypeDecl> =
        program.type_decls.iter().map(|t| (t.name.clone(), t.clone())).collect();

    let mut fn_infos = Vec::new();
    for f in &program.functions {
        // Exported functions only; a body-less `extern` import has no surface,
        // and flattened `impl` methods (mangled names) are never exported.
        if !f.exported || f.is_extern {
            continue;
        }
        fn_infos.push(fn_info_lit(f, &types));
    }

    let mut type_infos = Vec::new();
    for t in &program.type_decls {
        // Exported, non-injected (line 0), non-synthetic (`Name.field`) decls.
        if !t.exported || t.line == 0 || t.name.contains('.') {
            continue;
        }
        type_infos.push(type_info_lit(t, &types));
    }

    struct_lit(
        "ModuleInterface",
        vec![("functions", array_lit(fn_infos)), ("types", array_lit(type_infos))],
    )
}

fn fn_info_lit(f: &Function, types: &HashMap<String, TypeDecl>) -> Expr {
    let params: Vec<Expr> = f
        .params
        .iter()
        .map(|p| {
            struct_lit(
                "ParamInfo",
                vec![
                    ("name", Expr::Str(p.name.clone())),
                    ("spelling", Expr::Str(p.ty.to_string())),
                    ("schema", schema_lit_for_type(&p.ty, types)),
                ],
            )
        })
        .collect();
    // Unit return spells as "" (the RFC's convention), everything else by its
    // ordinary Vyrn spelling.
    let ret_spelling = if f.ret == Type::Unit { String::new() } else { f.ret.to_string() };
    struct_lit(
        "FnInfo",
        vec![
            ("name", Expr::Str(f.name.clone())),
            ("params", array_lit(params)),
            ("ret", Expr::Str(ret_spelling)),
            ("retSchema", schema_lit_for_type(&f.ret, types)),
        ],
    )
}

fn type_info_lit(t: &TypeDecl, types: &HashMap<String, TypeDecl>) -> Expr {
    struct_lit(
        "TypeInfo",
        vec![
            ("name", Expr::Str(t.name.clone())),
            ("source", Expr::Str(render_type_decl(t, types))),
            ("schema", crate::types::schema_struct_lit(t)),
        ],
    )
}

/// A `Schema` literal for an arbitrary type: a declared validated/record type
/// reflects through [`crate::types::schema_struct_lit`]; a plain type gets a
/// minimal schema carrying just its spelling.
fn schema_lit_for_type(ty: &Type, types: &HashMap<String, TypeDecl>) -> Expr {
    if let Type::Named(n) = ty {
        if let Some(decl) = types.get(n) {
            return crate::types::schema_struct_lit(decl);
        }
    }
    let spelling = ty.to_string();
    struct_lit(
        "Schema",
        vec![
            ("name", Expr::Str(spelling.clone())),
            ("base", Expr::Str(spelling)),
            ("doc", none()),
            ("min", none()),
            ("max", none()),
            ("multipleOf", none()),
            ("minLength", none()),
            ("maxLength", none()),
            ("pattern", none()),
        ],
    )
}

/// Render a type declaration back to canonical Vyrn source (lossless enough for
/// a generator to re-emit contract types verbatim). Inline field refinements —
/// stored as synthetic `Parent.field` decls — are folded back into the record.
pub fn render_type_decl(t: &TypeDecl, types: &HashMap<String, TypeDecl>) -> String {
    let mut out = String::new();
    if t.exported {
        out.push_str("export ");
    }
    out.push_str("type ");
    out.push_str(&t.name);
    if !t.type_params.is_empty() {
        out.push('<');
        out.push_str(&t.type_params.join(", "));
        out.push('>');
    }
    out.push_str(" = ");
    match &t.base {
        Type::Record(fields) => {
            out.push_str("{ ");
            for (i, fld) in fields.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&fld.name);
                out.push_str(": ");
                out.push_str(&render_field_type(&t.name, &fld.name, &fld.ty, types));
            }
            out.push_str(" }");
        }
        Type::Enum(variants) => {
            let rendered: Vec<String> = variants
                .iter()
                .map(|v| {
                    if v.payload.is_empty() {
                        v.name.clone()
                    } else {
                        let ps: Vec<String> = v.payload.iter().map(|p| p.to_string()).collect();
                        format!("{}({})", v.name, ps.join(", "))
                    }
                })
                .collect();
            out.push_str("| ");
            out.push_str(&rendered.join(" | "));
        }
        base => {
            out.push_str(&base.to_string());
            if let Some(pred) = &t.predicate {
                out.push_str(" where ");
                out.push_str(&crate::checker::pred_summary(pred));
            }
        }
    }
    out
}

/// Render a record field's type, folding a synthetic `Parent.field` refinement
/// back into inline `Base where <pred>` form.
fn render_field_type(
    parent: &str,
    field: &str,
    ty: &Type,
    types: &HashMap<String, TypeDecl>,
) -> String {
    if let Type::Named(n) = ty {
        if n == &format!("{parent}.{field}") {
            if let Some(decl) = types.get(n) {
                let base = decl.base.to_string();
                return match &decl.predicate {
                    Some(p) => format!("{base} where {}", crate::checker::pred_summary(p)),
                    None => base,
                };
            }
        }
    }
    ty.to_string()
}

// ---- literal builders -----------------------------------------------------

fn struct_lit(name: &str, fields: Vec<(&str, Expr)>) -> Expr {
    Expr::StructLit {
        name: name.to_string(),
        fields: fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        line: 0,
    }
}

fn array_lit(elems: Vec<Expr>) -> Expr {
    Expr::ArrayLit { elems, line: 0 }
}

fn none() -> Expr {
    Expr::Var { name: "None".to_string(), line: 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types_of(src: &str) -> HashMap<String, TypeDecl> {
        let (p, _) = crate::parser::parse_accum(crate::lexer::lex(src).unwrap());
        p.type_decls.into_iter().map(|t| (t.name.clone(), t)).collect()
    }
    fn decl(src: &str, name: &str) -> (TypeDecl, HashMap<String, TypeDecl>) {
        let types = types_of(src);
        (types[name].clone(), types)
    }

    #[test]
    fn renders_validated_scalar_with_predicate() {
        let (d, t) = decl("export type Id = Int64 where value >= 1\n", "Id");
        assert_eq!(render_type_decl(&d, &t), "export type Id = Int64 where value >= 1");
    }

    #[test]
    fn renders_record_folding_inline_refinements() {
        let (d, t) = decl(
            "export type User = { name: String where value.length >= 3, age: Int64 }\n",
            "User",
        );
        assert_eq!(
            render_type_decl(&d, &t),
            "export type User = { name: String where value.length >= 3, age: Int64 }"
        );
    }

    #[test]
    fn renders_enum() {
        let (d, t) = decl("export type Shape = | Circle(Int64) | Dot\n", "Shape");
        assert_eq!(render_type_decl(&d, &t), "export type Shape = | Circle(Int64) | Dot");
    }

    /// Pull a named field out of a StructLit for assertions.
    fn field<'a>(e: &'a Expr, name: &str) -> &'a Expr {
        match e {
            Expr::StructLit { fields, .. } => {
                &fields.iter().find(|(k, _)| k == name).expect("field").1
            }
            other => panic!("expected a struct literal, got {other:?}"),
        }
    }
    fn str_of(e: &Expr) -> &str {
        match e {
            Expr::Str(s) => s,
            other => panic!("expected a string, got {other:?}"),
        }
    }
    fn elems(e: &Expr) -> &[Expr] {
        match e {
            Expr::ArrayLit { elems, .. } => elems,
            other => panic!("expected an array literal, got {other:?}"),
        }
    }

    #[test]
    fn module_interface_captures_exported_surface() {
        let src = "export type Id = Int64 where value >= 1 \
                   export fn ping(id: Id, times: Int64) -> String { return \"pong\" } \
                   fn hidden() -> Int64 { return 0 }";
        let (program, _) = crate::parser::parse_accum(crate::lexer::lex(src).unwrap());
        let iface = module_interface_lit(&program);

        // functions: only the exported `ping`.
        let fns = elems(field(&iface, "functions"));
        assert_eq!(fns.len(), 1);
        assert_eq!(str_of(field(&fns[0], "name")), "ping");
        assert_eq!(str_of(field(&fns[0], "ret")), "String");
        let params = elems(field(&fns[0], "params"));
        assert_eq!(params.len(), 2);
        assert_eq!(str_of(field(&params[0], "name")), "id");
        assert_eq!(str_of(field(&params[0], "spelling")), "Id");
        // The `Id` param's schema reflects its `where` bound (min = 1).
        let sch = field(&params[0], "schema");
        assert_eq!(str_of(field(sch, "name")), "Id");

        // types: only the exported `Id`, with its canonical source text.
        let tys = elems(field(&iface, "types"));
        assert_eq!(tys.len(), 1);
        assert_eq!(str_of(field(&tys[0], "name")), "Id");
        assert_eq!(str_of(field(&tys[0], "source")), "export type Id = Int64 where value >= 1");
    }
}
