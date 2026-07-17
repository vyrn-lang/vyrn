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

use std::collections::{HashMap, HashSet};

use crate::ast::*;

/// Build the `ModuleInterface` record literal for the reflected module's exported
/// surface — its **reachable type closure** (RFC-0031).
///
/// `program` is the *linked* program rooted at the reflected module: the reflected
/// module's own declarations carry `module == None`, every transitively imported
/// module's carry `module == Some(key)` (RFC-0010 attribution). The interface is:
///
/// * `functions` — the reflected module's OWN exported functions only (functions
///   of imported modules are not part of the interface);
/// * `types` — every named type declaration reachable from those functions'
///   parameter/return spellings, walking transitively through record fields, enum
///   payloads, alias/validated bases, and generic arguments, **regardless of which
///   module declares it** — plus the reflected module's own exported-but-unreachable
///   declarations (today's behavior, kept).
///
/// Order is locked: own declarations first (source order), then foreign closure
/// entries in linker order (the merged program already lays foreign decls out
/// after own ones, in linker order — RFC-0010 `link`). A same-name collision
/// across modules is already a `load` error, so the closure never holds two
/// distinct decls of one name.
/// `specifiers` maps a declaration's owning module (`TypeDecl.module` — `None` for
/// the reflected module itself, `Some(key)` for an imported module) to the import
/// specifier a generator should use to reach that module from the reflected
/// module's importer (RFC-0031). A missing entry falls back to the empty string.
pub fn module_interface_lit(program: &Program, specifiers: &HashMap<Option<String>, String>) -> Expr {
    // Global type table across every linked module (name -> declaration).
    let types: HashMap<String, TypeDecl> =
        program.type_decls.iter().map(|t| (t.name.clone(), t.clone())).collect();

    // Closure roots: the reflected (root) module's own exported functions. A
    // body-less `extern` import has no surface, flattened `impl` methods carry
    // mangled names, and an imported module's function (`module.is_some()`) is
    // not part of this interface.
    let is_root_fn = |f: &Function| f.exported && !f.is_extern && f.module.is_none();

    let mut fn_infos = Vec::new();
    for f in &program.functions {
        if is_root_fn(f) {
            fn_infos.push(fn_info_lit(f, &types));
        }
    }

    // Reachable-type closure: seed from every root function's parameter/return
    // spellings, then walk each declaration's structure for further named types.
    let mut reachable: HashSet<String> = HashSet::new();
    let mut work: Vec<String> = Vec::new();
    for f in &program.functions {
        if is_root_fn(f) {
            for p in &f.params {
                collect_type_names(&p.ty, &mut work);
            }
            collect_type_names(&f.ret, &mut work);
        }
    }
    while let Some(n) = work.pop() {
        if !reachable.insert(n.clone()) {
            continue;
        }
        if let Some(decl) = types.get(&n) {
            // The declaration's structure (record fields / enum payloads /
            // alias & validated base / generic args) reaches further types.
            // The predicate references `value`, never a type — nothing to add.
            collect_type_names(&decl.base, &mut work);
        }
    }

    let mut type_infos = Vec::new();
    for t in &program.type_decls {
        // Exported, non-injected (line 0), non-synthetic (`Name.field`) decls.
        if !t.exported || t.line == 0 || t.name.contains('.') {
            continue;
        }
        // Own declarations are always included (today's behavior); foreign ones
        // only when the closure reaches them.
        if t.module.is_none() || reachable.contains(&t.name) {
            let spec = specifiers.get(&t.module).map(|s| s.as_str()).unwrap_or("");
            type_infos.push(type_info_lit(t, spec, &types));
        }
    }

    struct_lit(
        "ModuleInterface",
        vec![("functions", array_lit(fn_infos)), ("types", array_lit(type_infos))],
    )
}

/// Collect every named type referenced anywhere in `ty` (the head of a `Named`/
/// `App`, plus every nested position) into `out` — the closure walk's edge set.
fn collect_type_names(ty: &Type, out: &mut Vec<String>) {
    match ty {
        Type::Named(n) => out.push(n.clone()),
        Type::App(n, args) => {
            out.push(n.clone());
            for a in args {
                collect_type_names(a, out);
            }
        }
        Type::Option(a)
        | Type::Ref(a)
        | Type::Array(a)
        | Type::Task(a)
        | Type::Partial(a)
        | Type::ArrayN(a, _)
        | Type::Omit(a, _)
        | Type::Pick(a, _) => collect_type_names(a, out),
        Type::Result(a, b) | Type::Merge(a, b) | Type::Map(a, b) => {
            collect_type_names(a, out);
            collect_type_names(b, out);
        }
        Type::Record(fields) => {
            for f in fields {
                collect_type_names(&f.ty, out);
            }
        }
        Type::Enum(variants) => {
            for v in variants {
                for p in &v.payload {
                    collect_type_names(p, out);
                }
            }
        }
        Type::Fn(params, ret) => {
            for p in params {
                collect_type_names(p, out);
            }
            collect_type_names(ret, out);
        }
        // Primitives, type parameters, loggers, and the error sentinel name no
        // declarations.
        _ => {}
    }
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

fn type_info_lit(t: &TypeDecl, module_spec: &str, types: &HashMap<String, TypeDecl>) -> Expr {
    struct_lit(
        "TypeInfo",
        vec![
            ("name", Expr::Str(t.name.clone())),
            ("source", Expr::Str(render_type_decl(t, types))),
            ("module", Expr::Str(module_spec.to_string())),
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
        let iface = module_interface_lit(&program, &HashMap::new());

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

    // ---- reachable type closure across modules (RFC-0031) ------------------

    /// Link `files` (keyed by module path, `main` is the root) and reflect the
    /// root. Returns the built `ModuleInterface` literal.
    fn reflect_linked(files: &[(&str, &str)], root: &str) -> Expr {
        let map: std::collections::HashMap<String, String> =
            files.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let resolver = crate::loader::MapResolver(map.clone());
        let program = crate::loader::load(&map[root], root, &Default::default(), &resolver)
            .expect("link");
        let mut specs: HashMap<Option<String>, String> = HashMap::new();
        specs.insert(None, format!("./{root}"));
        for t in &program.type_decls {
            if let Some(k) = &t.module {
                specs.entry(Some(k.clone())).or_insert_with(|| {
                    format!("./{}", k.strip_suffix(".vyrn").unwrap_or(k))
                });
            }
        }
        module_interface_lit(&program, &specs)
    }

    fn type_names_of(iface: &Expr) -> Vec<String> {
        elems(field(iface, "types"))
            .iter()
            .map(|t| str_of(field(t, "name")).to_string())
            .collect()
    }

    #[test]
    fn closure_walks_records_enums_aliases_and_generics_across_modules() {
        // `contract` names only `Req`/`Wrap` in signatures; the walk must reach
        // `Book` (record field), `Id` (validated base of a field), `Shape` (enum
        // payload) and `Inner` (generic arg), all declared in `wire`.
        let wire = "\
            export type Id = Int64 where value >= 1\n\
            export type Inner = { n: Int64 }\n\
            export type Shape = | Circle(Id) | Dot\n\
            export type Book = { id: Id, shape: Shape }\n\
            export type Req = { book: Book }\n\
            export type Wrap = Array<Inner>\n\
            export type Unused = { x: Int64 }\n";
        let contract = "\
            import { Req, Wrap } from \"./wire\"\n\
            export fn make(r: Req) -> Wrap { return [] }\n";
        let iface = reflect_linked(&[("wire.vyrn", wire), ("contract.vyrn", contract)], "contract.vyrn");
        let names = type_names_of(&iface);
        // Reached: Req, Wrap, Book, Id, Shape, Inner. NOT the imported-but-
        // unreferenced `Unused`.
        for want in ["Req", "Wrap", "Book", "Id", "Shape", "Inner"] {
            assert!(names.contains(&want.to_string()), "closure missing {want}: {names:?}");
        }
        assert!(!names.contains(&"Unused".to_string()), "dragged in Unused: {names:?}");
    }

    #[test]
    fn own_decls_come_first_then_foreign_in_source_order() {
        // The contract declares `Local` (own, unreferenced) and names foreign
        // `A`/`B` in signatures. Own decls lead; foreign follow in wire order.
        let wire = "export type A = { x: Int64 }\nexport type B = { y: Int64 }\n";
        let contract = "\
            import { A, B } from \"./wire\"\n\
            export type Local = { z: Int64 }\n\
            export fn f(a: A) -> B { return B { y: 0 } }\n";
        let iface = reflect_linked(&[("wire.vyrn", wire), ("contract.vyrn", contract)], "contract.vyrn");
        assert_eq!(type_names_of(&iface), vec!["Local", "A", "B"]);
    }

    #[test]
    fn foreign_types_carry_their_declaring_module_specifier() {
        let wire = "export type A = { x: Int64 }\n";
        let contract = "\
            import { A } from \"./wire\"\n\
            export type Own = { z: Int64 }\n\
            export fn f(a: A) -> Own { return Own { z: 0 } }\n";
        let iface = reflect_linked(&[("wire.vyrn", wire), ("contract.vyrn", contract)], "contract.vyrn");
        let tys = elems(field(&iface, "types"));
        // `Own` is the reflected module's own type → the root specifier.
        let own = tys.iter().find(|t| str_of(field(t, "name")) == "Own").unwrap();
        assert_eq!(str_of(field(own, "module")), "./contract.vyrn");
        // `A` is foreign → its declaring module's specifier.
        let a = tys.iter().find(|t| str_of(field(t, "name")) == "A").unwrap();
        assert_eq!(str_of(field(a, "module")), "./wire");
    }

    #[test]
    fn only_the_reflected_modules_functions_are_reflected() {
        // `wire` exports a function too; it must NOT appear in the interface.
        let wire = "\
            export type A = { x: Int64 }\n\
            export fn helper() -> A { return A { x: 0 } }\n";
        let contract = "\
            import { A } from \"./wire\"\n\
            export fn f(a: A) -> A { return a }\n";
        let iface = reflect_linked(&[("wire.vyrn", wire), ("contract.vyrn", contract)], "contract.vyrn");
        let fns = elems(field(&iface, "functions"));
        let fn_names: Vec<&str> = fns.iter().map(|f| str_of(field(f, "name"))).collect();
        assert_eq!(fn_names, vec!["f"]);
    }
}
