//! Module reflection for generator imports.
//!
//! `moduleInterface(path)` hands a generator the structured shape of a
//! module's exported surface, as `schemaOf` does for one type. The compiler
//! builds a record literal (an [`Expr`]) here, so the ordinary record, array
//! and coercion machinery evaluates it. The shapes are injected by the parser:
//! ```text
//! ModuleInterface { functions: Array<FnInfo>, types: Array<TypeInfo> }
//! FnInfo   { name: String, params: Array<ParamInfo>, ret: String, retSchema: Schema, retUncodable: String, mutates: Bool, origin: Origin }
//! ParamInfo{ name: String, spelling: String, schema: Schema, uncodable: String }
//! TypeInfo { name: String, source: String, module: String, schema: Schema, origin: Origin, shape: Array<TypeNode> }
//! TypeNode { kind: String, name: String, spelling: String, args: Array<Int64>, members: Array<TypeMember>, predicate: String }
//! Origin   { file: String, line: Int64, col: Int64, name: String }
//! ```
//! `ret` and `spelling` are type spellings; `TypeInfo.source` is the canonical
//! `type` declaration text; `uncodable` and `retUncodable` are
//! [`crate::codec`]'s verdict on crossing a JSON wire; `mutates` is the
//! author's `mut fn` marker; `origin` is where the declaration is written;
//! `shape` is the declaration's base as a tree (`push_node`).
//!
//! `contractOf(Name)` reflects a `contract` declaration the same
//! way, so `std/contract:checkContract` compares expectation against reality
//! in Vyrn:
//! ```text
//! MemberInfo   { name, kind, spelling, params: Array<String>, ret, optional, doc }
//! ContractInfo { name, module, doc, open, members: Array<MemberInfo> }
//! ```

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::codec::Wire;

/// Name columns per module, for the `Origin` of every reflected declaration.
///
/// The AST carries only a line, so the column comes from the lexer, once per
/// module: the first identifier token spelled like the declaration, on its
/// line, is its name. Lexing, not a text search, keeps a comment or string
/// that contains the name from matching. Keys are the loader's module
/// attribution, `None` for the reflected module. An unplaced name gets column
/// 0, as in [`crate::symbols::Symbol`].
#[derive(Default)]
pub struct Origins {
    files: HashMap<Option<String>, String>,
    cols: HashMap<Option<String>, HashMap<(usize, String), usize>>,
}

impl Origins {
    /// Indexes `sources`: module key (`None` for the root), file name and source
    /// text.
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

    /// Returns the `Origin` literal for `name`, declared on `line` of `module`.
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
                ("file", Expr::str(file)),
                ("line", Expr::int(line as i64)),
                ("col", Expr::int(col as i64)),
                ("name", Expr::str(name)),
            ],
        )
    }
}

/// Builds the `ModuleInterface` literal for the reflected module's reachable
/// type closure.
///
/// `program` is linked and rooted at the reflected module, whose declarations
/// have `module == None`. `functions` holds the root's own exported functions;
/// `types` holds every named type reachable from their signatures through
/// fields, payloads, bases and generic arguments, whichever module declares
/// it, plus the root's own exported types. Own declarations come first in
/// source order, then foreign ones in linker order; `load` refuses a name
/// declared twice. `specifiers` maps a declaration's module to the import
/// specifier a generator uses to reach it; a missing entry gives `""`.
pub fn module_interface_lit(
    program: &Program,
    specifiers: &HashMap<Option<String>, String>,
    origins: &Origins,
) -> Expr {
    let types: HashMap<String, TypeDecl> = program
        .type_decls
        .iter()
        .map(|t| (t.name.clone(), t.clone()))
        .collect();

    // Roots: the reflected module's own exported functions. A body-less `extern`
    // has no surface.
    let is_root_fn = |f: &Function| f.exported && !f.is_extern && f.module.is_none();

    let mut fn_infos = Vec::new();
    for f in &program.functions {
        if is_root_fn(f) {
            fn_infos.push(fn_info_lit(f, &types, origins));
        }
    }

    // Seed the closure from the roots' signatures, then walk declarations.
    let mut reachable: HashSet<String> = HashSet::new();
    let mut work: Vec<String> = Vec::new();
    for f in &program.functions {
        if is_root_fn(f) {
            for p in &f.params {
                crate::loader::type_heads(&p.ty, &mut |n| work.push(n.clone()));
            }
            crate::loader::type_heads(&f.ret, &mut |n| work.push(n.clone()));
        }
    }
    while let Some(n) = work.pop() {
        if !reachable.insert(n.clone()) {
            continue;
        }
        if let Some(decl) = types.get(&n) {
            // A predicate references `value`, never a type, so only the base adds names.
            crate::loader::type_heads(&decl.base, &mut |n| work.push(n.clone()));
        }
    }

    let mut type_infos = Vec::new();
    for t in &program.type_decls {
        // Skip injected (line 0) and synthetic (`Name.field`) declarations.
        if !t.exported || t.line == 0 || is_synthetic(&t.name) {
            continue;
        }
        // Own declarations always; foreign ones when the closure reaches them.
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

/// Builds the `ContractInfo` literal for a contract declaration.
///
/// This is all the compiler knows of contracts: which exports a contract
/// demands and how a mismatch is reported is `std/contract:checkContract`'s
/// policy, in Vyrn, so a third-party generator can replace it. A default
/// expression is not reflected: a generator needs to know a default exists
/// (`optional`), not its value.
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
                    // A `Unit` return spells as `""`, as in `FnInfo.ret`.
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
                    ("name", Expr::str(m.name.clone())),
                    ("kind", Expr::str(kind)),
                    ("spelling", Expr::str(m.spelling())),
                    (
                        "params",
                        array_lit(params.into_iter().map(Expr::str).collect()),
                    ),
                    ("ret", Expr::str(ret)),
                    ("optional", Expr::Bool(m.optional(), Id::NEW)),
                    ("variadic", Expr::Bool(variadic, Id::NEW)),
                    ("doc", opt_str(m.doc.as_deref())),
                ],
            )
        })
        .collect();
    struct_lit(
        "ContractInfo",
        vec![
            ("name", Expr::str(c.name.clone())),
            ("module", Expr::str(c.module.clone().unwrap_or_default())),
            ("doc", opt_str(c.doc.as_deref())),
            ("open", Expr::Bool(c.open_rule().is_some(), Id::NEW)),
            ("members", array_lit(members)),
        ],
    )
}

fn fn_info_lit(f: &Function, types: &HashMap<String, TypeDecl>, origins: &Origins) -> Expr {
    let params: Vec<Expr> = f
        .params
        .iter()
        .map(|p| {
            struct_lit(
                "ParamInfo",
                vec![
                    ("name", Expr::str(p.name.clone())),
                    ("spelling", Expr::str(p.ty.to_string())),
                    ("schema", schema_lit_for_type(&p.ty, types)),
                    ("uncodable", Expr::str(uncodable_of(&p.ty, types, true))),
                ],
            )
        })
        .collect();
    // A `Unit` return spells as `""`.
    let ret_spelling = if f.ret == Type::Unit {
        String::new()
    } else {
        f.ret.to_string()
    };
    struct_lit(
        "FnInfo",
        vec![
            ("name", Expr::str(f.name.clone())),
            ("params", array_lit(params)),
            ("ret", Expr::str(ret_spelling)),
            ("retSchema", schema_lit_for_type(&f.ret, types)),
            (
                "retUncodable",
                Expr::str(uncodable_of(&f.ret, types, false)),
            ),
            ("mutates", Expr::Bool(f.is_mut, Id::NEW)),
            ("origin", origins.lit(&f.module, &f.name, f.line)),
        ],
    )
}

/// Returns the first part of `ty` that cannot cross the wire, or `""`:
/// [`crate::codec`]'s verdict, the rule `toJson` and `fromJson` use, so a
/// generator need not scan spellings for `fn(`.
///
/// `decode` picks the direction: a parameter is decoded, a return encoded, and
/// the two differ (a fixed `Array<T, N>` encodes but cannot be decoded).
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
            ("name", Expr::str(t.name.clone())),
            ("source", Expr::str(render_type_decl(t, types))),
            ("module", Expr::str(module_spec)),
            ("schema", crate::types::schema_struct_lit(t)),
            ("origin", origins.lit(&t.module, &t.name, t.line)),
            ("shape", shape_lit(t, types)),
        ],
    )
}

/// Builds `TypeInfo.shape`: the declaration's base flattened into `TypeNode`s,
/// parents before children, node 0 the root.
fn shape_lit(t: &TypeDecl, types: &HashMap<String, TypeDecl>) -> Expr {
    let mut nodes = Vec::new();
    let pred = t.predicate.as_ref().map(crate::checker::pred_summary);
    push_node(&t.base, pred, types, &mut nodes);
    array_lit(nodes)
}

/// Appends the node for `ty` and its descendants to `nodes`, and returns its
/// index and its spelling. `pred` is the `where` written on `ty`.
///
/// Recursion follows the written type expression, never a declaration, so it
/// ends: a declared name is a `named` leaf. The one declaration it enters is
/// the synthetic `Parent.field` of an inline field refinement, whose base is
/// part of the written expression. A record or enum spells itself from its
/// members, so such a field spells as written, not as `Parent.field`.
fn push_node(
    ty: &Type,
    pred: Option<String>,
    types: &HashMap<String, TypeDecl>,
    nodes: &mut Vec<Expr>,
) -> (i64, String) {
    if let Some(d) = synthetic_decl(ty, |n| types.get(n)) {
        let pred = d.predicate.as_ref().map(crate::checker::pred_summary);
        return push_node(&d.base, pred, types, nodes);
    }
    let fields = |fs: &[Field]| {
        fs.iter()
            .map(|f| (f.name.clone(), vec![f.ty.clone()]))
            .collect()
    };
    let (kind, name, args, members): (&str, &str, Vec<Type>, Vec<(String, Vec<Type>)>) = match ty {
        Type::Int
        | Type::IntN { .. }
        | Type::Float
        | Type::Float32
        | Type::Bool
        | Type::Str
        | Type::Unit => ("", "", Vec::new(), Vec::new()),
        Type::Named(n) => ("named", n, Vec::new(), Vec::new()),
        Type::App(n, ts) => ("named", n, ts.clone(), Vec::new()),
        Type::Param(n) => ("param", n, Vec::new(), Vec::new()),
        Type::Array(e) | Type::ArrayN(e, _) | Type::SmallArray(e, _) => {
            ("array", "", vec![(**e).clone()], Vec::new())
        }
        Type::Map(k, v) => ("map", "", vec![(**k).clone(), (**v).clone()], Vec::new()),
        Type::Record(fs) => ("record", "", Vec::new(), fields(fs)),
        Type::Omit(..) | Type::Pick(..) | Type::Merge(..) | Type::Partial(..) => {
            let fs = crate::types::record_fields(ty, types).unwrap_or_default();
            ("record", "", Vec::new(), fields(&fs))
        }
        Type::Enum(vs) => match (
            crate::types::option_payload(ty),
            crate::types::result_payloads(ty),
        ) {
            (Some(t), _) => ("option", "", vec![t.clone()], Vec::new()),
            (_, Some((ok, err))) => ("result", "", vec![ok.clone(), err.clone()], Vec::new()),
            _ => {
                let vs = vs.iter().map(|v| (v.name.clone(), v.payload.clone()));
                ("enum", "", Vec::new(), vs.collect())
            }
        },
        _ => ("other", "", Vec::new(), Vec::new()),
    };
    let at = nodes.len();
    nodes.push(none());
    let mut kids = |ts: &[Type]| -> (Expr, Vec<String>) {
        let (ix, spelled): (Vec<Expr>, Vec<String>) = ts
            .iter()
            .map(|t| {
                let (i, s) = push_node(t, None, types, nodes);
                (Expr::int(i), s)
            })
            .unzip();
        (array_lit(ix), spelled)
    };
    let (args, _) = kids(&args);
    let mut lits = Vec::new();
    let mut spelled = Vec::new();
    for (m, ts) in members {
        let (margs, s) = kids(&ts);
        lits.push(struct_lit(
            "TypeMember",
            vec![("name", Expr::str(m.clone())), ("args", margs)],
        ));
        spelled.push(match (kind, s.is_empty()) {
            ("record", _) => format!("{m}: {}", s.join("")),
            (_, true) => m,
            (_, false) => format!("{m}({})", s.join(", ")),
        });
    }
    let written = match kind {
        "record" => format!("{{ {} }}", spelled.join(", ")),
        "enum" => format!("| {}", spelled.join(" | ")),
        _ => ty.to_string(),
    };
    let spelling = match &pred {
        Some(p) => format!("{written} where {p}"),
        None => written.clone(),
    };
    nodes[at] = struct_lit(
        "TypeNode",
        vec![
            (
                "kind",
                Expr::str(if kind.is_empty() {
                    written
                } else {
                    kind.to_string()
                }),
            ),
            ("name", Expr::str(name)),
            ("spelling", Expr::str(spelling.clone())),
            ("args", args),
            ("members", array_lit(lits)),
            ("predicate", Expr::str(pred.unwrap_or_default())),
        ],
    );
    (at as i64, spelling)
}

/// Builds the `TypeArg` literal for the types a derived-code generator's call
/// sites need, and returns it with the node of each root, `None` for a root
/// with no node.
///
/// A node is one checked type, identified by [`crate::types::struct_key`]; its
/// kind is [`crate::codec::wire`]'s verdict in the encode direction, the rule
/// `toJson` uses. A root whose walk reaches a type with no wire form or no
/// source spelling (an anonymous enum, a bare `lazy`) gets no node, and nothing
/// its walk added stays: the call site refuses it, and a program that never
/// reaches it must not fail.
pub fn type_arg_lit(roots: &[Type], types: &HashMap<String, TypeDecl>) -> (Expr, Vec<Option<i64>>) {
    let mut w = ArgWalk {
        types,
        nodes: Vec::new(),
        at: HashMap::new(),
    };
    let mut placed = Vec::new();
    for ty in roots {
        let (len, at) = (w.nodes.len(), w.at.clone());
        match w.node(ty) {
            Some(i) => placed.push(Some(i as i64)),
            None => {
                w.nodes.truncate(len);
                w.at = at;
                placed.push(None);
            }
        }
    }
    let mut rooted: Vec<i64> = Vec::new();
    for i in placed.iter().flatten() {
        if !rooted.contains(i) {
            rooted.push(*i);
        }
    }
    let rooted = rooted.into_iter().map(|i| Expr::int(i)).collect();
    let lit = struct_lit(
        "TypeArg",
        vec![("roots", array_lit(rooted)), ("nodes", array_lit(w.nodes))],
    );
    (lit, placed)
}

/// The placeholder prefix a spelling writes for a name of `std/json`'s, which
/// source cannot spell.
const PH: &str = "VyrnRt_";

/// Each placeholder prefix generated source writes for a reserved name it
/// cannot spell, and the prefix [`crate::gen::derive`] folds it onto:
/// `std/json`'s names, `std/jsondec`'s, and a `where` type's predicate
/// ([`crate::ctor::pred_name`]).
pub(crate) const PLACEHOLDERS: &[(&str, &str)] = &[
    (PH, crate::loader::RT_PREFIX),
    ("VyrnRd_", crate::loader::JSONDEC_PREFIX),
    ("VyrnWp_", crate::ctor::PRED_PREFIX),
];

/// Spells a type as generated source, with `std/json`'s `$` names folded onto
/// [`PH`].
fn spell(ty: &Type) -> String {
    ty.to_string().replace(crate::loader::RT_PREFIX, PH)
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

struct ArgWalk<'a> {
    types: &'a HashMap<String, TypeDecl>,
    nodes: Vec<Expr>,
    /// Node index by `struct_key`, with the type: the key is 64 bits, and two
    /// types on one key reflect as neither. A type is entered before its
    /// children, so a recursive type finds its own node.
    at: HashMap<String, (usize, Type)>,
}

impl ArgWalk<'_> {
    fn node(&mut self, ty: &Type) -> Option<usize> {
        if (matches!(ty, Type::Enum(_)) && !crate::types::is_sum_alias(ty)) || unspellable_lazy(ty)
        {
            return None;
        }
        let key = crate::types::struct_key(ty);
        if let Some((i, t)) = self.at.get(&key) {
            return (t == ty).then_some(*i);
        }
        let i = self.nodes.len();
        self.nodes.push(none());
        self.at.insert(key.clone(), (i, ty.clone()));
        if let Some(decl) = self.refined(ty) {
            let base = self.node(&decl.base)?;
            let binds = crate::types::predicate_binds(&decl)
                .into_iter()
                .filter(|(_, _, field)| field.is_some())
                .map(|(n, _, _)| member(n, Vec::new()))
                .collect();
            self.nodes[i] = type_node(
                "where",
                &key,
                ty,
                vec![base],
                binds,
                crate::trap::validation_of(&decl),
            );
            return Some(i);
        }
        let scalar = |t: Type| (t.to_string(), Vec::new(), Vec::new());
        let (kind, args, members): (String, Vec<usize>, Vec<(String, Vec<usize>)>) =
            match crate::codec::wire(ty, self.types, false).ok()? {
                Wire::Int => scalar(Type::Int),
                Wire::IntN { bits, signed } => scalar(Type::IntN { bits, signed }),
                Wire::Float => scalar(Type::Float),
                Wire::Float32 => scalar(Type::Float32),
                Wire::Bool => scalar(Type::Bool),
                Wire::Str => scalar(Type::Str),
                Wire::Option(t) => ("option".into(), vec![self.node(&t)?], Vec::new()),
                Wire::Array(t) | Wire::FixedArray(t, _) => {
                    ("array".into(), vec![self.node(&t)?], Vec::new())
                }
                Wire::Map(v) => (
                    "map".into(),
                    vec![self.node(&Type::Str)?, self.node(&v)?],
                    Vec::new(),
                ),
                Wire::MapI(v) => (
                    "map".into(),
                    vec![self.node(&Type::Int)?, self.node(&v)?],
                    Vec::new(),
                ),
                Wire::Record(fs) => {
                    let mut ms = Vec::new();
                    for f in fs {
                        let n = self.node(&crate::types::forced(&f.ty))?;
                        ms.push((f.name, vec![n]));
                    }
                    ("record".into(), Vec::new(), ms)
                }
                Wire::Enum(vs) => {
                    let mut ms = Vec::new();
                    for v in vs {
                        let mut ps = Vec::new();
                        for p in &v.payload {
                            ps.push(self.node(p)?);
                        }
                        ms.push((v.name, ps));
                    }
                    ("enum".into(), Vec::new(), ms)
                }
            };
        let members = members.into_iter().map(|(n, ix)| member(n, ix)).collect();
        self.nodes[i] = type_node(&kind, &key, ty, args, members, String::new());
        Some(i)
    }

    /// The declaration of `ty` when it names a `where` type, which reflects as
    /// its own node over its base.
    fn refined(&self, ty: &Type) -> Option<TypeDecl> {
        match ty {
            Type::Named(n) => self.types.get(n).filter(|d| d.predicate.is_some()).cloned(),
            _ => None,
        }
    }
}

fn ints(ix: Vec<usize>) -> Expr {
    array_lit(ix.into_iter().map(|i| Expr::int(i as i64)).collect())
}

fn member(name: String, args: Vec<usize>) -> Expr {
    struct_lit(
        "TypeMember",
        vec![("name", Expr::str(name)), ("args", ints(args))],
    )
}

fn type_node(
    kind: &str,
    key: &str,
    ty: &Type,
    args: Vec<usize>,
    members: Vec<Expr>,
    predicate: String,
) -> Expr {
    struct_lit(
        "TypeNode",
        vec![
            ("kind", Expr::str(kind)),
            ("name", Expr::str(format!("t{key}"))),
            ("spelling", Expr::str(spell(ty))),
            ("args", ints(args)),
            ("members", array_lit(members)),
            ("predicate", Expr::str(predicate)),
        ],
    )
}

/// Returns a `Schema` literal for any type: a declared type reflects through
/// [`crate::types::schema_struct_lit`], any other gets its spelling alone.
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
            ("name", Expr::str(spelling.clone())),
            ("base", Expr::str(spelling)),
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

/// Renders a type declaration as canonical Vyrn source, so a generator can
/// re-emit it. Synthetic `Parent.field` refinements fold back into the record.
fn render_type_decl(t: &TypeDecl, types: &HashMap<String, TypeDecl>) -> String {
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
                out.push_str(&field_text(fld, |n| types.get(n), &|t| t.to_string()));
            }
            out.push_str(" }");
            // A cross-field `where` stays on the record declaration; dropping it would
            // lose the validation on re-emission.
            if let Some(pred) = &t.predicate {
                out.push_str(" where ");
                out.push_str(&crate::checker::pred_summary(pred));
            }
        }
        // A declared variant list. An alias of a built-in sum spells itself
        // `Result<T, E>` in the arm below, as the module wrote it.
        Type::Enum(variants) if !crate::types::is_sum_alias(&t.base) => {
            let rendered: Vec<String> = variants
                .iter()
                .map(|v| variant_arm(v, &|t| t.to_string()))
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

/// The synthetic refinement declaration (`User.age`) that `ty` names, if it
/// is one (see `is_synthetic`). `decl` looks a declaration up by name.
pub(crate) fn synthetic_decl<'a>(
    ty: &Type,
    decl: impl Fn(&str) -> Option<&'a TypeDecl>,
) -> Option<&'a TypeDecl> {
    match ty {
        Type::Named(n) if is_synthetic(n) => decl(n),
        _ => None,
    }
}

/// A record field as the author wrote it: `name: Type`, or `name: Base where
/// pred` when its type is a synthetic refinement. `spell` renders a type.
pub(crate) fn field_text<'a>(
    f: &Field,
    decl: impl Fn(&str) -> Option<&'a TypeDecl>,
    spell: &dyn Fn(&Type) -> String,
) -> String {
    match synthetic_decl(&f.ty, decl) {
        Some(TypeDecl {
            base,
            predicate: Some(p),
            ..
        }) => format!(
            "{}: {} where {}",
            f.name,
            spell(base),
            crate::checker::pred_summary(p)
        ),
        _ => format!("{}: {}", f.name, spell(&f.ty)),
    }
}

/// An enum arm: `Name`, or `Name(T, U)` for a payload. `spell` renders a type.
pub(crate) fn variant_arm(v: &EnumVariant, spell: &dyn Fn(&Type) -> String) -> String {
    if v.payload.is_empty() {
        v.name.clone()
    } else {
        let ps: Vec<String> = v.payload.iter().map(spell).collect();
        format!("{}({})", v.name, ps.join(", "))
    }
}

fn struct_lit(name: &str, fields: Vec<(&str, Expr)>) -> Expr {
    Expr::StructLit {
        id: Id::NEW,
        name: name.to_string(),
        fields: fields
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
        line: 0,
    }
}

fn array_lit(elems: Vec<Expr>) -> Expr {
    Expr::ArrayLit {
        id: Id::NEW,
        elems,
        line: 0,
    }
}

fn none() -> Expr {
    Expr::var("None", 0)
}

fn opt_str(s: Option<&str>) -> Expr {
    match s {
        Some(v) => Expr::call("Some", vec![Expr::str(v)], 0),
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

    /// The loader renames a record's parent (`Foo__from0`) but not its
    /// synthetic refinement (`Foo.x`), so the fold must not rebuild the name from
    /// the parent.
    #[test]
    fn renders_a_renamed_record_folding_its_refinements() {
        let (mut d, mut t) = decl(
            "export type Foo = { x: Int64 where value > 0 }
",
            "Foo",
        );
        d.name = "Foo__from0".to_string();
        t.insert(d.name.clone(), d.clone());
        assert_eq!(
            render_type_decl(&d, &t),
            "export type Foo__from0 = { x: Int64 where value > 0 }"
        );
    }

    /// A cross-field `where` survives rendering.
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
            Expr::Str(s, _) => s,
            other => panic!("expected a string, got {other:?}"),
        }
    }
    fn elems(e: &Expr) -> &[Expr] {
        match e {
            Expr::ArrayLit { elems, .. } => elems,
            other => panic!("expected an array literal, got {other:?}"),
        }
    }

    /// One row per node: kind, name, spelling, args, and the members' names.
    fn shape_rows(e: &Expr) -> Vec<String> {
        let ints = |e: &Expr| -> Vec<i64> {
            elems(e)
                .iter()
                .map(|x| match x {
                    Expr::Int(n, _) => *n,
                    other => panic!("expected an int, got {other:?}"),
                })
                .collect()
        };
        elems(e)
            .iter()
            .map(|n| {
                let ms: Vec<String> = elems(field(n, "members"))
                    .iter()
                    .map(|m| format!("{}{:?}", str_of(field(m, "name")), ints(field(m, "args"))))
                    .collect();
                format!(
                    "{} {} `{}` {:?} {}",
                    str_of(field(n, "kind")),
                    str_of(field(n, "name")),
                    str_of(field(n, "spelling")),
                    ints(field(n, "args")),
                    ms.join(" ")
                )
            })
            .collect()
    }

    #[test]
    fn shape_flattens_the_written_type_and_stops_at_names() {
        let (d, t) = decl(
            "type Id = Int64\n\
             type R = { a: Array<Option<Id>>, b: Int64 where value > 0, \
             c: Map<String, Result<Bool, String>> }\n",
            "R",
        );
        assert_eq!(
            shape_rows(&shape_lit(&d, &t)),
            [
                "record  `{ a: Array<Option<Id>>, b: Int64 where value > 0, c: Map<String, Result<Bool, String>> }` [] a[1] b[4] c[5]",
                "array  `Array<Option<Id>>` [2] ",
                "option  `Option<Id>` [3] ",
                "named Id `Id` [] ",
                "Int64  `Int64 where value > 0` [] ",
                "map  `Map<String, Result<Bool, String>>` [6, 7] ",
                "String  `String` [] ",
                "result  `Result<Bool, String>` [8, 9] ",
                "Bool  `Bool` [] ",
                "String  `String` [] ",
            ]
        );
        let (d, t) = decl("type Id = Int64\ntype E = | X | Y(Id, UInt8)\n", "E");
        assert_eq!(
            shape_rows(&shape_lit(&d, &t)),
            [
                "enum  `| X | Y(Id, UInt8)` [] X[] Y[1, 2]",
                "named Id `Id` [] ",
                "UInt8  `UInt8` [] ",
            ]
        );
    }

    #[test]
    fn type_arg_is_one_graph_over_the_roots() {
        let t = types_of("type Node = { v: Int64, kids: Array<Node> }\n");
        let node = Type::Named("Node".into());
        let unencodable = Type::Fn(Vec::new(), Box::new(Type::Int));
        let (lit, placed) = type_arg_lit(
            &[node.clone(), Type::option(Type::Str), unencodable, node],
            &t,
        );
        assert_eq!(placed, [Some(0), Some(3), None, Some(0)]);
        let ints: Vec<String> = elems(field(&lit, "roots"))
            .iter()
            .map(|e| format!("{e:?}"))
            .collect();
        assert_eq!(ints, ["Int(0, _)", "Int(3, _)"]);
        let rows: Vec<String> = shape_rows(field(&lit, "nodes"))
            .into_iter()
            .map(|r| {
                let (kind, rest) = r.split_once(' ').unwrap();
                let (_key, rest) = rest.split_once(' ').unwrap();
                format!("{kind} {rest}")
            })
            .collect();
        assert_eq!(
            rows,
            [
                "record `Node` [] v[1] kids[2]",
                "Int64 `Int64` [] ",
                "array `Array<Node>` [0] ",
                "option `Option<String>` [4] ",
                "String `String` [] ",
            ]
        );
    }

    #[test]
    fn module_interface_captures_exported_surface() {
        let src = "export type Id = Int64 where value >= 1 \
                   export fn ping(id: Id, times: Int64) -> String { return \"pong\" } \
                   fn hidden() -> Int64 { return 0 }";
        let (program, _) = crate::parser::parse_accum(crate::lexer::lex(src).unwrap());
        let origins = Origins::new([(None, "m.vyrn", src)]);
        let iface = module_interface_lit(&program, &HashMap::new(), &origins);

        let fns = elems(field(&iface, "functions"));
        assert_eq!(fns.len(), 1);
        assert_eq!(str_of(field(&fns[0], "name")), "ping");
        assert_eq!(str_of(field(&fns[0], "ret")), "String");
        let params = elems(field(&fns[0], "params"));
        assert_eq!(params.len(), 2);
        assert_eq!(str_of(field(&params[0], "name")), "id");
        assert_eq!(str_of(field(&params[0], "spelling")), "Id");
        let sch = field(&params[0], "schema");
        assert_eq!(str_of(field(sch, "name")), "Id");

        let tys = elems(field(&iface, "types"));
        assert_eq!(tys.len(), 1);
        assert_eq!(str_of(field(&tys[0], "name")), "Id");
        assert_eq!(
            str_of(field(&tys[0], "source")),
            "export type Id = Int64 where value >= 1"
        );
    }

    /// Checks an origin by reading the source at it: `file:line:col` must be where
    /// its name is written.
    fn assert_points_at(src: &str, origin: &Expr) {
        let line: usize = match field(origin, "line") {
            Expr::Int(n, _) => *n as usize,
            other => panic!("line is not an int: {other:?}"),
        };
        let col: usize = match field(origin, "col") {
            Expr::Int(n, _) => *n as usize,
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
        // A comment mentioning `ping` defeats a substring search, a doc comment moves
        // the declaration line, and a type follows the function.
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
        assert_ne!(str_of(field(&a, "name")), str_of(field(&b, "name")));
        assert_ne!(field(&a, "line"), field(&b, "line"));
    }

    /// Links `files` (keyed by module path) and reflects `root`.
    fn reflect_linked(files: &[(&str, &str)], root: &str) -> Expr {
        let map: std::collections::HashMap<String, String> = files
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let resolver = crate::loader::MapResolver(map.clone());
        let program = crate::loader::load(&map[root], root, &Default::default(), &resolver, None)
            .expect("link");
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
        // Signatures name only `Req` and `Wrap`; the walk must reach `Book` (field),
        // `Id` (a field's base), `Shape` (payload) and `Inner` (generic argument).
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
        // Own `Local` is unreferenced but leads; foreign `A` and `B` follow in wire
        // order.
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
        let own = tys
            .iter()
            .find(|t| str_of(field(t, "name")) == "Own")
            .unwrap();
        assert_eq!(str_of(field(own, "module")), "./contract.vyrn");
        let a = tys
            .iter()
            .find(|t| str_of(field(t, "name")) == "A")
            .unwrap();
        assert_eq!(str_of(field(a, "module")), "./wire");
    }

    #[test]
    fn only_the_reflected_modules_functions_are_reflected() {
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
