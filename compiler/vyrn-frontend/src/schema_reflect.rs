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
//! FnInfo   { name: String, params: Array<ParamInfo>, ret: String, retSchema: Schema, retUncodable: String, mutates: Bool, origin: Origin }
//! ParamInfo{ name: String, spelling: String, schema: Schema, uncodable: String }
//! TypeInfo { name: String, source: String, module: String, schema: Schema, origin: Origin }
//! Origin   { file: String, line: Int64, col: Int64, name: String }
//! ```
//! `ret`/`spelling` are the raw type *spellings* (for stub emission); a
//! `TypeInfo.source` is the canonical `type` declaration text (for verbatim
//! re-emission of contract types); the `Schema` values carry the RFC-0009
//! reflection (bounds/pattern/length); `uncodable`/`retUncodable` carry
//! [`crate::codec`]'s verdict on whether that end can cross a JSON wire, so a
//! generator asks the compiler instead of guessing from a name (RFC-0071 M3);
//! `mutates` is the `mut fn` marker, the same move for "does this change state"
//! (RFC-0074 M4a) — declared by the author, never inferred; `origin` is where
//! the declaration was WRITTEN (RFC-0073 M1), so a generated symbol can name the
//! declaration it stands for instead of only the line it was emitted from.
//!
//! RFC-0071 adds the mirror image: `contractOf(Name)` reflects a `contract`
//! declaration — what a module is *expected* to export — into the same kind of
//! plain record, so `std/contract:checkContract` can compare expectation against
//! reality in ordinary Vyrn code:
//! ```text
//! MemberInfo   { name, kind, spelling, params: Array<String>, ret, optional, doc }
//! ContractInfo { name, module, doc, open, members: Array<MemberInfo> }
//! ```

use std::collections::{HashMap, HashSet};

use crate::ast::*;

/// Name columns per module, for the `Origin` on every reflected declaration
/// (RFC-0073 M1).
///
/// The AST carries a `line` per declaration and nothing else — [`crate::symbols`]
/// notes why (threading spans through every node construction site is high churn
/// for something two consumers want), and recovers the column from the lexer's
/// per-token `(line, col)` instead. This does the same, once per module rather
/// than once per lookup: the *first* identifier token spelled like the
/// declaration, on the declaration's line, is its name. Lexing again is a second
/// pass over source the loader already read, which is a comptime cost on a cached
/// artifact; re-lexing is what keeps a comment or a string that happens to
/// contain the name from being mistaken for it.
///
/// Keys are the loader's module attribution (`Function::module` /
/// `TypeDecl::module`) — `None` for the reflected module itself. A module with no
/// entry, or a name the lexer cannot place, yields column 0, the same "not
/// located" answer [`crate::symbols::Symbol`] gives.
#[derive(Default)]
pub struct Origins {
    files: HashMap<Option<String>, String>,
    cols: HashMap<Option<String>, HashMap<(usize, String), usize>>,
}

impl Origins {
    /// Index `sources` — module key (`None` = the reflected root) to that
    /// module's file name and source text.
    pub fn new<'a>(sources: impl IntoIterator<Item = (Option<String>, &'a str, &'a str)>) -> Self {
        let mut out = Origins::default();
        for (key, file, src) in sources {
            out.files.insert(key.clone(), file.to_string());
            let mut cols: HashMap<(usize, String), usize> = HashMap::new();
            if let Ok(tokens) = crate::lexer::lex(src) {
                for t in tokens {
                    if let crate::lexer::Tok::Ident(s) = &t.tok {
                        cols.entry((t.line, s.clone())).or_insert(t.col);
                    }
                }
            }
            out.cols.insert(key, cols);
        }
        out
    }

    /// The `Origin` literal for a declaration named `name`, declared on `line` of
    /// module `module`.
    fn lit(&self, module: &Option<String>, name: &str, line: usize) -> Expr {
        let file = self
            .files
            .get(module)
            .cloned()
            .or_else(|| module.clone())
            .unwrap_or_default();
        let col = self
            .cols
            .get(module)
            .and_then(|m| m.get(&(line, name.to_string())))
            .copied()
            .unwrap_or(0);
        struct_lit(
            "Origin",
            vec![
                ("file", Expr::Str(file)),
                ("line", Expr::Int(line as i64)),
                ("col", Expr::Int(col as i64)),
                ("name", Expr::Str(name.to_string())),
            ],
        )
    }
}

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
pub fn module_interface_lit(
    program: &Program,
    specifiers: &HashMap<Option<String>, String>,
    origins: &Origins,
) -> Expr {
    // Global type table across every linked module (name -> declaration).
    let types: HashMap<String, TypeDecl> = program
        .type_decls
        .iter()
        .map(|t| (t.name.clone(), t.clone()))
        .collect();

    // Closure roots: the reflected (root) module's own exported functions. A
    // body-less `extern` import has no surface, flattened `impl` methods carry
    // mangled names, and an imported module's function (`module.is_some()`) is
    // not part of this interface.
    let is_root_fn = |f: &Function| f.exported && !f.is_extern && f.module.is_none();

    let mut fn_infos = Vec::new();
    for f in &program.functions {
        if is_root_fn(f) {
            fn_infos.push(fn_info_lit(f, &types, origins));
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
            type_infos.push(type_info_lit(t, spec, &types, origins));
        }
    }

    struct_lit(
        "ModuleInterface",
        vec![
            ("functions", array_lit(fn_infos)),
            ("types", array_lit(type_infos)),
        ],
    )
}

/// Build the `ContractInfo` record literal for a contract declaration
/// (RFC-0071) — the *expectation* side of module reflection, mirroring
/// [`module_interface_lit`].
///
/// This is the whole of the compiler's contract knowledge: it turns a
/// [`ContractDecl`] into a record of strings and booleans. Which exports a
/// contract demands, how a near-miss is reported, whether a mismatch is fatal —
/// none of that lives here. `std/contract:checkContract` decides it in ordinary
/// Vyrn code, so a third-party generator can replace the policy without touching
/// the compiler, which is the point of the RFC.
///
/// The default *expression* of an optional member is deliberately not reflected:
/// a generator needs to know that a default exists (`optional`), not what it
/// evaluates to — the module that omits the member gets the default by writing
/// it, not by the generator materializing it.
pub fn contract_info_lit(c: &ContractDecl) -> Expr {
    let members: Vec<Expr> = c
        .members
        .iter()
        .map(|m| {
            let (kind, params, ret, variadic) = match &m.kind {
                ContractMemberKind::Value { ty, .. } => ("let", Vec::new(), ty.to_string(), false),
                ContractMemberKind::Fn {
                    params,
                    ret,
                    variadic,
                    ..
                } => (
                    "fn",
                    params.iter().map(|p| p.to_string()).collect(),
                    // A `Unit` return spells as `""`, matching `FnInfo.ret`.
                    if *ret == Type::Unit {
                        String::new()
                    } else {
                        ret.to_string()
                    },
                    *variadic,
                ),
            };
            struct_lit(
                "MemberInfo",
                vec![
                    ("name", Expr::Str(m.name.clone())),
                    ("kind", Expr::Str(kind.to_string())),
                    ("spelling", Expr::Str(m.spelling())),
                    (
                        "params",
                        array_lit(params.into_iter().map(Expr::Str).collect()),
                    ),
                    ("ret", Expr::Str(ret)),
                    ("optional", Expr::Bool(m.optional())),
                    ("variadic", Expr::Bool(variadic)),
                    ("doc", opt_str(m.doc.as_deref())),
                ],
            )
        })
        .collect();
    struct_lit(
        "ContractInfo",
        vec![
            ("name", Expr::Str(c.name.clone())),
            ("module", Expr::Str(c.module.clone().unwrap_or_default())),
            ("doc", opt_str(c.doc.as_deref())),
            ("open", Expr::Bool(c.open_rule().is_some())),
            ("members", array_lit(members)),
        ],
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
        Type::Array(a)
        | Type::Task(a)
        | Type::Stream(a)
        | Type::Partial(a)
        | Type::ArrayN(a, _)
        | Type::SmallArray(a, _)
        | Type::Omit(a, _)
        | Type::Pick(a, _)
        // A `lazy T` field (RFC-0085 M4a) reaches `T` — the deferral changes
        // when the value is computed, not which declarations an interface's
        // type closure has to carry (RFC-0031).
        | Type::Lazy(a) => collect_type_names(a, out),
        Type::Merge(a, b) | Type::Map(a, b) => {
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

fn fn_info_lit(f: &Function, types: &HashMap<String, TypeDecl>, origins: &Origins) -> Expr {
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
                    ("uncodable", Expr::Str(uncodable_of(&p.ty, types, true))),
                ],
            )
        })
        .collect();
    // Unit return spells as "" (the RFC's convention), everything else by its
    // ordinary Vyrn spelling.
    let ret_spelling = if f.ret == Type::Unit {
        String::new()
    } else {
        f.ret.to_string()
    };
    struct_lit(
        "FnInfo",
        vec![
            ("name", Expr::Str(f.name.clone())),
            ("params", array_lit(params)),
            ("ret", Expr::Str(ret_spelling)),
            ("retSchema", schema_lit_for_type(&f.ret, types)),
            (
                "retUncodable",
                Expr::Str(uncodable_of(&f.ret, types, false)),
            ),
            ("mutates", Expr::Bool(f.is_mut)),
            ("origin", origins.lit(&f.module, &f.name, f.line)),
        ],
    )
}

/// The first type inside `ty` that cannot cross the wire, or `""` when the whole
/// of it can — [`crate::codec`]'s own answer, reflected (RFC-0071 M3).
///
/// A generator that puts a value on a JSON wire needs to know this, and the only
/// alternatives were a name-membership test (which admits a record with a
/// function-typed field, since the record itself is perfectly nameable) or
/// scanning `TypeInfo.source` for `fn(` — a scanner, which is the practice
/// RFC-0071 exists to end. So the compiler answers instead, with the same
/// predicate that governs `toJson`/`fromJson`; there is no second rule to drift.
///
/// `decode` picks the direction: a procedure's PARAMETER is a decode target and
/// its RETURN is encoded, and the two domains genuinely differ (a fixed
/// `Array<T, N>` encodes but cannot be decoded into).
fn uncodable_of(ty: &Type, types: &HashMap<String, TypeDecl>, decode: bool) -> String {
    let r = if decode {
        crate::codec::decodable(ty, types)
    } else {
        crate::codec::encodable(ty, types)
    };
    r.err().unwrap_or_default()
}

fn type_info_lit(
    t: &TypeDecl,
    module_spec: &str,
    types: &HashMap<String, TypeDecl>,
    origins: &Origins,
) -> Expr {
    struct_lit(
        "TypeInfo",
        vec![
            ("name", Expr::Str(t.name.clone())),
            ("source", Expr::Str(render_type_decl(t, types))),
            ("module", Expr::Str(module_spec.to_string())),
            ("schema", crate::types::schema_struct_lit(t)),
            ("origin", origins.lit(&t.module, &t.name, t.line)),
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
            // A cross-field `where` stays on the record decl (the parser keeps
            // it there), so dropping it here handed generators a source text
            // whose re-emission silently lost the validation.
            if let Some(pred) = &t.predicate {
                out.push_str(" where ");
                out.push_str(&crate::checker::pred_summary(pred));
            }
        }
        // A DECLARED variant list. An alias of a built-in sum is a variant
        // list too since RFC-0126 §8.15's M5, and it spells itself
        // `Result<T, E>` through the arm below, which is the source text
        // the module wrote and a generator re-emits.
        Type::Enum(variants) if !crate::types::is_sum_alias(&t.base) => {
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
        fields: fields
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
        line: 0,
    }
}

fn array_lit(elems: Vec<Expr>) -> Expr {
    Expr::ArrayLit { elems, line: 0 }
}

fn none() -> Expr {
    Expr::Var {
        name: "None".to_string(),
        line: 0,
    }
}

/// `Some("..")` / `None` for an optional string field.
fn opt_str(s: Option<&str>) -> Expr {
    match s {
        Some(v) => Expr::Call {
            name: "Some".to_string(),
            args: vec![Expr::Str(v.to_string())],
            line: 0,
        },
        None => none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types_of(src: &str) -> HashMap<String, TypeDecl> {
        let (p, _) = crate::parser::parse_accum(crate::lexer::lex(src).unwrap());
        p.type_decls
            .into_iter()
            .map(|t| (t.name.clone(), t))
            .collect()
    }
    fn decl(src: &str, name: &str) -> (TypeDecl, HashMap<String, TypeDecl>) {
        let types = types_of(src);
        (types[name].clone(), types)
    }

    #[test]
    fn renders_validated_scalar_with_predicate() {
        let (d, t) = decl("export type Id = Int64 where value >= 1\n", "Id");
        assert_eq!(
            render_type_decl(&d, &t),
            "export type Id = Int64 where value >= 1"
        );
    }

    #[test]
    fn renders_record_folding_inline_refinements() {
        let (d, t) = decl(
            "export type User = { name: String where value.byteLength >= 3, age: Int64 }\n",
            "User",
        );
        assert_eq!(
            render_type_decl(&d, &t),
            "export type User = { name: String where value.byteLength >= 3, age: Int64 }"
        );
    }

    /// A cross-field `where` stays on the record decl (the parser keeps it
    /// there), so the rendered source must keep it too — the old output handed
    /// generators a type whose re-emission silently lost the validation.
    #[test]
    fn renders_record_cross_field_where() {
        let (d, t) = decl(
            "export type R = { lo: Int64, hi: Int64 } where value.lo < value.hi\n",
            "R",
        );
        assert_eq!(
            render_type_decl(&d, &t),
            "export type R = { lo: Int64, hi: Int64 } where value.lo < value.hi"
        );
    }

    #[test]
    fn renders_enum() {
        let (d, t) = decl("export type Shape = | Circle(Int64) | Dot\n", "Shape");
        assert_eq!(
            render_type_decl(&d, &t),
            "export type Shape = | Circle(Int64) | Dot"
        );
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
        let origins = Origins::new([(None, "m.vyrn", src)]);
        let iface = module_interface_lit(&program, &HashMap::new(), &origins);

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
        assert_eq!(
            str_of(field(&tys[0], "source")),
            "export type Id = Int64 where value >= 1"
        );
    }

    /// An origin is checked by READING the source at it: `file:line:col` must be
    /// where the name it carries is written (RFC-0073 M1). Asserting the numbers
    /// would only restate them.
    fn assert_points_at(src: &str, origin: &Expr) {
        let line: usize = match field(origin, "line") {
            Expr::Int(n) => *n as usize,
            other => panic!("line is not an int: {other:?}"),
        };
        let col: usize = match field(origin, "col") {
            Expr::Int(n) => *n as usize,
            other => panic!("col is not an int: {other:?}"),
        };
        let name = str_of(field(origin, "name"));
        let text = src.lines().nth(line - 1).expect("line in range");
        let at: String = text
            .chars()
            .skip(col - 1)
            .take(name.chars().count())
            .collect();
        assert_eq!(at, name, "origin {line}:{col} does not point at `{name}`");
    }

    #[test]
    fn origins_point_at_the_declarations_they_name() {
        // A leading comment mentioning `ping` (so a substring search would be
        // wrong), a doc comment (so the decl line is not the doc line), and a
        // type below the function (so both kinds are covered).
        let src = "// ping is declared below, not here\n\
                   \n\
                   /// Pong.\n\
                   export fn ping(id: Id) -> String { return \"pong\" }\n\
                   export type Id = Int64 where value >= 1\n";
        let (program, _) = crate::parser::parse_accum(crate::lexer::lex(src).unwrap());
        let origins = Origins::new([(None, "m.vyrn", src)]);
        let iface = module_interface_lit(&program, &HashMap::new(), &origins);

        let f = &elems(field(&iface, "functions"))[0];
        let o = field(f, "origin");
        assert_eq!(str_of(field(o, "file")), "m.vyrn");
        assert_points_at(src, o);

        let t = &elems(field(&iface, "types"))[0];
        assert_points_at(src, field(t, "origin"));
    }

    #[test]
    fn a_renamed_declaration_moves_its_origin() {
        let one = "export fn ping() -> String { return \"\" }\n";
        let two = "\n\nexport fn pong() -> String { return \"\" }\n";
        let origin_of = |src: &str| {
            let (p, _) = crate::parser::parse_accum(crate::lexer::lex(src).unwrap());
            let iface =
                module_interface_lit(&p, &HashMap::new(), &Origins::new([(None, "m.vyrn", src)]));
            field(&elems(field(&iface, "functions"))[0], "origin").clone()
        };
        let a = origin_of(one);
        let b = origin_of(two);
        assert_points_at(one, &a);
        assert_points_at(two, &b);
        // The rename moved the name, and the two blank lines moved the line.
        assert_ne!(str_of(field(&a, "name")), str_of(field(&b, "name")));
        assert_ne!(field(&a, "line"), field(&b, "line"));
    }

    // ---- reachable type closure across modules (RFC-0031) ------------------

    /// Link `files` (keyed by module path, `main` is the root) and reflect the
    /// root. Returns the built `ModuleInterface` literal.
    fn reflect_linked(files: &[(&str, &str)], root: &str) -> Expr {
        let map: std::collections::HashMap<String, String> = files
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let resolver = crate::loader::MapResolver(map.clone());
        let program =
            crate::loader::load(&map[root], root, &Default::default(), &resolver).expect("link");
        let mut specs: HashMap<Option<String>, String> = HashMap::new();
        specs.insert(None, format!("./{root}"));
        for t in &program.type_decls {
            if let Some(k) = &t.module {
                specs
                    .entry(Some(k.clone()))
                    .or_insert_with(|| format!("./{}", k.strip_suffix(".vyrn").unwrap_or(k)));
            }
        }
        let mut srcs: Vec<(Option<String>, &str, &str)> = Vec::new();
        for (k, v) in files {
            let key = if *k == root {
                None
            } else {
                Some(k.to_string())
            };
            srcs.push((key, k, v));
        }
        let origins = Origins::new(srcs);
        module_interface_lit(&program, &specs, &origins)
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
        let iface = reflect_linked(
            &[("wire.vyrn", wire), ("contract.vyrn", contract)],
            "contract.vyrn",
        );
        let names = type_names_of(&iface);
        // Reached: Req, Wrap, Book, Id, Shape, Inner. NOT the imported-but-
        // unreferenced `Unused`.
        for want in ["Req", "Wrap", "Book", "Id", "Shape", "Inner"] {
            assert!(
                names.contains(&want.to_string()),
                "closure missing {want}: {names:?}"
            );
        }
        assert!(
            !names.contains(&"Unused".to_string()),
            "dragged in Unused: {names:?}"
        );
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
        let iface = reflect_linked(
            &[("wire.vyrn", wire), ("contract.vyrn", contract)],
            "contract.vyrn",
        );
        assert_eq!(type_names_of(&iface), vec!["Local", "A", "B"]);
    }

    #[test]
    fn foreign_types_carry_their_declaring_module_specifier() {
        let wire = "export type A = { x: Int64 }\n";
        let contract = "\
            import { A } from \"./wire\"\n\
            export type Own = { z: Int64 }\n\
            export fn f(a: A) -> Own { return Own { z: 0 } }\n";
        let iface = reflect_linked(
            &[("wire.vyrn", wire), ("contract.vyrn", contract)],
            "contract.vyrn",
        );
        let tys = elems(field(&iface, "types"));
        // `Own` is the reflected module's own type → the root specifier.
        let own = tys
            .iter()
            .find(|t| str_of(field(t, "name")) == "Own")
            .unwrap();
        assert_eq!(str_of(field(own, "module")), "./contract.vyrn");
        // `A` is foreign → its declaring module's specifier.
        let a = tys
            .iter()
            .find(|t| str_of(field(t, "name")) == "A")
            .unwrap();
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
        let iface = reflect_linked(
            &[("wire.vyrn", wire), ("contract.vyrn", contract)],
            "contract.vyrn",
        );
        let fns = elems(field(&iface, "functions"));
        let fn_names: Vec<&str> = fns.iter().map(|f| str_of(field(f, "name"))).collect();
        assert_eq!(fn_names, vec!["f"]);
    }
}
