//! The editor's view of a module contract: which contract governs
//! a file, then completion, hover, go-to-definition and did-you-mean over the
//! resolved declaration.
//!
//! Every query reads an ordinary [`crate::ast::ContractDecl`], so a
//! third-party contract gets the same editor support as `Page` or
//! `Component`. The LSP calls [`roles_for_project`], [`load_contract`] and the
//! query functions and maps their results to LSP shapes; it holds no contract
//! knowledge.

use crate::ast::{
    ContractDecl, ContractMember, ContractMemberKind, Expr, ImportSource, Program, Type,
};
use crate::loader::{LoadOptions, ModuleResolver};
use crate::schema::Json;

/// Where a role applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoleScope {
    /// A run of path segments, as `vyrn.json`'s `roles` map declares it: a
    /// module under consecutive directories with these names is in the role. A
    /// run (`"server/api"`) composes the audience axis with the role
    /// axis; one segment is the plain case.
    Segment(String),
    /// A resolved directory, found by discovery from the directory a generator
    /// was pointed at (`pages("./routes")`).
    Dir(String),
}

impl std::fmt::Display for RoleScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RoleScope::Segment(s) if s.contains('/') => {
                write!(f, "path segments `{s}` (vyrn.json `roles`)")
            }
            RoleScope::Segment(s) => write!(f, "path segment `{s}` (vyrn.json `roles`)"),
            RoleScope::Dir(d) => write!(f, "directory {d} (from the generator call site)"),
        }
    }
}

/// One directory role: the contract that governs the modules under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Role {
    pub scope: RoleScope,
    /// The contract's module as a reader types it (`std/ui`, `./gen`).
    pub module: String,
    /// The module's resolved file, when discovery resolved the generator's own
    /// import, so `./gen` is not re-resolved against a page elsewhere. `None` for
    /// a manifest role, whose specifier resolves against the manifest.
    pub module_file: Option<String>,
    pub contract: String,
    /// File stems in scope that are not modules of the role: `layout.vyx` and
    /// `error.vyx` in `routes/` are chrome with no contract, so offering
    /// `head`/`data` in them would misfire. The default mirrors `std/ui`'s
    /// `uiScanAll`; `vyrn.json` overrides it per role.
    pub except: Vec<String>,
}

/// The stems a role excludes by default: `std/ui`'s chrome. See
/// [`Role::except`].
const DEFAULT_ROLE_EXCEPT: &[&str] = &["layout", "error"];

/// Splits `spec` as `module:Contract` (`"std/ui:Page"`). `None` without a
/// `:`; the caller ignores a malformed entry, and the generator's own check
/// reports a bad contract.
fn split_spec(spec: &str) -> Option<(String, String)> {
    let (m, c) = spec.rsplit_once(':')?;
    if m.is_empty() || c.is_empty() {
        return None;
    }
    Some((m.to_string(), c.to_string()))
}

/// Returns the roles in a `vyrn.json` document's `"roles"` key:
///
/// ```json
/// { "roles": { "routes": "std/ui:Page", "widgets": "std/vyx:Component" } }
/// ```
///
/// A value may be an object, `{ "contract": "std/ui:Page", "except":
/// ["layout", "error"] }`, to override the chrome stems; the string form
/// takes [`DEFAULT_ROLE_EXCEPT`]. An empty result means no `roles` key. It
/// takes the parsed document because a parse failure belongs to whoever read
/// the file.
pub fn roles_from_manifest(doc: &Json) -> Vec<Role> {
    let Some(Json::Obj(entries)) = doc.get("roles") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (segment, value) in entries {
        let (spec, except) = match value {
            Json::Str(s) => (
                s.clone(),
                DEFAULT_ROLE_EXCEPT.iter().map(|s| s.to_string()).collect(),
            ),
            Json::Obj(fields) => {
                let Some(Json::Str(spec)) =
                    fields.iter().find(|(k, _)| k == "contract").map(|(_, v)| v)
                else {
                    continue;
                };
                let except = match fields.iter().find(|(k, _)| k == "except").map(|(_, v)| v) {
                    Some(Json::Arr(items)) => items
                        .iter()
                        .filter_map(|i| match i {
                            Json::Str(s) => Some(s.clone()),
                            _ => None,
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                (spec.clone(), except)
            }
            _ => continue,
        };
        if let Some((module, contract)) = split_spec(&spec) {
            out.push(Role {
                scope: RoleScope::Segment(segment.clone()),
                module,
                module_file: None,
                contract,
                except,
            });
        }
    }
    out
}

/// Returns a project's roles: the ones its manifest declares, or else the
/// ones [`discovered_roles`] finds. `doc` is the manifest the caller already
/// read ([`crate::manifest::role_roots`] says why), and `roots` is that
/// function's answer.
pub fn roles_for_project(
    doc: Option<&Json>,
    roots: &[(String, String)],
    opts: &LoadOptions,
    resolver: &dyn ModuleResolver,
) -> Vec<Role> {
    if let Some(doc) = doc {
        let declared = roles_from_manifest(doc);
        if !declared.is_empty() {
            return declared;
        }
    }
    discovered_roles(roots, opts, resolver)
}

/// Returns the roles found from the generator call sites in `roots`, the
/// `(path, source)` pairs of a project's entry points. A root module that says
///
/// ```vyrn
/// import { pagesThemed } from "std/ui"
/// import { handle } from pagesThemed("./routes", "./theme.json")
/// ```
///
/// makes `./routes` a role governed by the one contract the generator's module
/// exports. A generator module that exports zero or several contracts is
/// skipped as ambiguous.
pub fn discovered_roles(
    roots: &[(String, String)],
    opts: &LoadOptions,
    resolver: &dyn ModuleResolver,
) -> Vec<Role> {
    let mut out: Vec<Role> = Vec::new();
    for (path, source) in roots {
        let Ok(tokens) = crate::lexer::lex(source) else {
            continue;
        };
        let (program, _) = crate::parser::parse_accum(tokens);
        for imp in &program.imports {
            let ImportSource::Generator { name, args, .. } = &imp.source else {
                continue;
            };
            let Some(Expr::Str(dir_spec, _)) = args.first() else {
                continue;
            };
            let Some(gen_module) = generator_module(&program, name) else {
                continue;
            };
            let Some((contract, gen_file)) =
                sole_exported_contract(&gen_module, path, opts, resolver)
            else {
                continue;
            };
            let Ok(dir) = crate::loader::resolve_spec(dir_spec, path, opts) else {
                continue;
            };
            // `resolve_spec` appends `.vyrn` to an extension-less specifier; the
            // argument is a directory, so strip it.
            let dir = dir.strip_suffix(".vyrn").unwrap_or(&dir).to_string();
            let role = Role {
                scope: RoleScope::Dir(dir),
                module: gen_module,
                module_file: Some(gen_file),
                contract,
                except: DEFAULT_ROLE_EXCEPT.iter().map(|s| s.to_string()).collect(),
            };
            if !out.contains(&role) {
                out.push(role);
            }
        }
    }
    out
}

/// Returns the specifier the generator `name` was imported from. A local
/// `gen fn` has none.
fn generator_module(program: &Program, name: &str) -> Option<String> {
    for imp in &program.imports {
        if let ImportSource::Path(spec) = &imp.source {
            if imp.names.iter().any(|n| n.local() == name) {
                return Some(spec.clone());
            }
        }
    }
    None
}

/// Returns the one contract a module exports and the module's file. `None`
/// for zero or several: ambiguity is not guessed.
fn sole_exported_contract(
    spec: &str,
    importer: &str,
    opts: &LoadOptions,
    resolver: &dyn ModuleResolver,
) -> Option<(String, String)> {
    let (program, (file, _)) = read_module(spec, importer, opts, resolver)?;
    let mut exported = program.contracts.iter().filter(|c| c.exported);
    let first = exported.next()?.name.clone();
    if exported.next().is_some() {
        return None;
    }
    Some((first, file))
}

/// Reads and parses the module `spec` names. Parse-only: resolving a contract
/// must not run generators or link, so a keystroke stays cheap.
fn read_module(
    spec: &str,
    importer: &str,
    opts: &LoadOptions,
    resolver: &dyn ModuleResolver,
) -> Option<(Program, (String, String))> {
    let resolved = crate::loader::resolve_spec(spec, importer, opts).ok()?;
    let source = resolver.read(&resolved).ok()?;
    let tokens = crate::lexer::lex(&source).ok()?;
    let (program, _) = crate::parser::parse_accum(tokens);
    Some((program, (resolved, source)))
}

/// Returns whether `path`'s file stem is dotted (`pastes.http.vyrn`).
///
/// A dotted stem marks a protocol projection over the modules beside it,
/// and every generator that scans a role's directory skips one:
/// `std/rpc`'s `rpcScan` tests for a `.` in the stem. Roles attach by
/// directory, so without this a projection would be graded against the
/// contract of the modules it projects.
pub fn is_projection(path: &str) -> bool {
    let file = path.replace('\\', "/");
    let file = file.rsplit('/').next().unwrap_or_default();
    file.rsplit_once('.')
        .is_some_and(|(stem, _)| stem.contains('.'))
}

/// Returns the role governing `path`, a slash-separated module path.
///
/// A file is in a role when its directory is, or is under, the role's scope,
/// and its stem is not one of the role's exceptions. The nearest scope wins,
/// scored by the index of its last matched component: the rule
/// [`crate::audience`] applies to audience segments, so the two path axes
/// agree on "more specific". A file with a dotted stem is in no role (see
/// [`is_projection`]).
pub fn role_for<'r>(path: &str, roles: &'r [Role]) -> Option<&'r Role> {
    if is_projection(path) {
        return None;
    }
    let path = path.replace('\\', "/");
    let stem = path
        .rsplit('/')
        .next()
        .and_then(|f| f.rsplit_once('.').map(|(s, _)| s).or(Some(f)))
        .unwrap_or("");
    let dir = match path.rsplit_once('/') {
        Some((d, _)) => d.to_string(),
        None => String::new(),
    };
    let comps: Vec<&str> = dir.split('/').filter(|c| !c.is_empty()).collect();
    let mut best: Option<(usize, &Role)> = None;
    for role in roles {
        let depth = match &role.scope {
            RoleScope::Segment(seg) => match last_run(&comps, seg) {
                Some(end) => end,
                None => continue,
            },
            RoleScope::Dir(d) => {
                let d = d.trim_end_matches('/');
                if dir == d || dir.starts_with(&format!("{d}/")) {
                    d.split('/').filter(|c| !c.is_empty()).count()
                } else {
                    continue;
                }
            }
        };
        if role.except.iter().any(|e| e == stem) {
            continue;
        }
        if best.map(|(b, _)| depth > b).unwrap_or(true) {
            best = Some((depth, role));
        }
    }
    best.map(|(_, r)| r)
}

/// Returns the 1-based index of the last component of the last consecutive
/// run of `scope`'s segments in `comps`. `"server/api"` matches `server`
/// followed by `api`.
fn last_run(comps: &[&str], scope: &str) -> Option<usize> {
    let want: Vec<&str> = scope.split('/').filter(|c| !c.is_empty()).collect();
    if want.is_empty() || want.len() > comps.len() {
        return None;
    }
    (0..=comps.len() - want.len())
        .filter(|&i| comps[i..i + want.len()] == want[..])
        .next_back()
        .map(|i| i + want.len())
}

/// One declared shape of a member; a name may carry several.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractShape {
    /// `"fn"` or `"let"`, as `std/contract`'s `Export.kind`.
    pub kind: &'static str,
    /// Return or value type spelling; `""` for a `Unit` return, as
    /// `std/contract` spells it.
    pub ret: String,
    /// The whole shape as `std/contract` spells it: `fn(T) -> Head`.
    pub spelling: String,
    /// Whether the shape has a default (`= noHead()`).
    pub optional: bool,
    /// `fn *(..)`: constrains the return type only.
    pub variadic: bool,
    pub line: usize,
    /// The declaration that satisfies this shape, as an LSP snippet, so the type
    /// is right before the user types anything.
    pub snippet: String,
}

/// One member of a resolved contract, with every shape its name is declared at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractMemberView {
    pub name: String,
    /// The member's `///` doc, from the first declaration of a repeated name.
    pub doc: Option<String>,
    /// Whether the module may omit the export: true when any shape has a default,
    /// as `std/contract`'s `nameOptional`.
    pub optional: bool,
    /// The first declaration's name position, for go-to-definition.
    pub line: usize,
    pub col: usize,
    pub end_col: usize,
    pub shapes: Vec<ContractShape>,
}

/// A contract declaration resolved for the editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractView {
    pub name: String,
    pub module: String,
    /// The declaring module's resolved file (slash path).
    pub file: String,
    pub doc: Option<String>,
    /// Named members in declaration order, one per distinct name.
    pub members: Vec<ContractMemberView>,
    /// The open rule (`fn *(..) -> R`). It admits exports of any name, so
    /// completion offers nothing for it.
    pub open_rule: Option<ContractShape>,
}

impl ContractView {
    pub fn member(&self, name: &str) -> Option<&ContractMemberView> {
        self.members.iter().find(|m| m.name == name)
    }
    /// Returns `contract `Page` (std/ui)`, the phrase every hover ends with.
    pub fn site(&self) -> String {
        if self.module.is_empty() {
            format!("contract `{}`", self.name)
        } else {
            format!("contract `{}` ({})", self.name, self.module)
        }
    }
}

/// Resolves `module_spec:contract_name` to a [`ContractView`], reading the
/// declaring module through `resolver`. Parse-only and link-free, because an
/// editor resolves a contract on every keystroke.
pub fn load_contract(
    module_spec: &str,
    contract_name: &str,
    importer: &str,
    opts: &LoadOptions,
    resolver: &dyn ModuleResolver,
) -> Option<ContractView> {
    let (program, (file, source)) = read_module(module_spec, importer, opts, resolver)?;
    let decl = program.contracts.iter().find(|c| c.name == contract_name)?;
    Some(view_of(decl, module_spec, &file, &source))
}

/// Resolves the contract a role names. A discovered role carries the file; a
/// manifest role's specifier resolves against `importer`, the manifest, not
/// against whichever page is open.
pub fn load_role_contract(
    role: &Role,
    importer: &str,
    opts: &LoadOptions,
    resolver: &dyn ModuleResolver,
) -> Option<ContractView> {
    let Some(file) = &role.module_file else {
        return load_contract(&role.module, &role.contract, importer, opts, resolver);
    };
    let source = resolver.read(file).ok()?;
    let tokens = crate::lexer::lex(&source).ok()?;
    let (program, _) = crate::parser::parse_accum(tokens);
    let decl = program.contracts.iter().find(|c| c.name == role.contract)?;
    Some(view_of(decl, &role.module, file, &source))
}

/// Builds the editor view of one declaration. Member name columns come from
/// the lexer, as in [`crate::symbols`]; the AST carries only a line.
fn view_of(decl: &ContractDecl, module_spec: &str, file: &str, source: &str) -> ContractView {
    let cols = name_columns(source);
    let mut members: Vec<ContractMemberView> = Vec::new();
    let mut open_rule = None;
    for m in &decl.members {
        let shape = shape_of(m);
        if m.is_open_rule() {
            open_rule.get_or_insert(shape);
            continue;
        }
        match members.iter_mut().find(|v| v.name == m.name) {
            Some(existing) => {
                existing.optional |= shape.optional;
                if existing.doc.is_none() {
                    existing.doc = m.doc.clone();
                }
                existing.shapes.push(shape);
            }
            None => {
                let (col, end_col) = cols
                    .get(&(m.line, m.name.clone()))
                    .copied()
                    .unwrap_or((0, 0));
                members.push(ContractMemberView {
                    name: m.name.clone(),
                    doc: m.doc.clone(),
                    optional: shape.optional,
                    line: m.line,
                    col,
                    end_col,
                    shapes: vec![shape],
                });
            }
        }
    }
    ContractView {
        name: decl.name.clone(),
        module: module_spec.to_string(),
        file: file.to_string(),
        doc: decl.doc.clone(),
        members,
        open_rule,
    }
}

/// Maps `(line, identifier)` to `(col, end_col)` of the first occurrence of
/// each identifier on each line.
fn name_columns(source: &str) -> std::collections::HashMap<(usize, String), (usize, usize)> {
    let mut out = std::collections::HashMap::new();
    let Ok(tokens) = crate::lexer::lex(source) else {
        return out;
    };
    for t in tokens {
        if let crate::lexer::Tok::Ident(s) = &t.tok {
            out.entry((t.line, s.clone()))
                .or_insert((t.col, t.col + s.chars().count()));
        }
    }
    out
}

/// Returns the type spelling `std/contract` compares: a `Unit` return is `""`
/// on both sides.
fn ret_spelling(ty: &Type) -> String {
    if *ty == Type::Unit {
        String::new()
    } else {
        ty.to_string()
    }
}

fn shape_of(m: &ContractMember) -> ContractShape {
    match &m.kind {
        ContractMemberKind::Value { ty, default } => ContractShape {
            kind: "let",
            ret: ty.to_string(),
            spelling: m.spelling(),
            optional: default.is_some(),
            variadic: false,
            line: m.line,
            // A module cannot `export let` (module state is private), so the
            // snippet is the accessor function `std/contract:matchIndex` accepts.
            snippet: if *ty == Type::Unit {
                format!("export fn {}() {{\n    $0\n}}", m.name)
            } else {
                format!(
                    "export fn {name}() -> {ty} {{\n    return $0\n}}",
                    name = m.name
                )
            },
        },
        ContractMemberKind::Fn {
            params,
            ret,
            default,
            variadic,
        } => ContractShape {
            kind: "fn",
            ret: ret_spelling(ret),
            spelling: m.spelling(),
            optional: default.is_some(),
            variadic: *variadic,
            line: m.line,
            snippet: fn_snippet(&m.name, params, ret, *variadic),
        },
    }
}

/// Returns the declaration that satisfies a `fn` shape, as an LSP snippet.
///
/// A contract fixes arity and types, not parameter names, and a member's type
/// parameters are open (a page writes `d: Array<Paste>`, not `d: T`). So each
/// parameter's name and type are tabstops seeded with the contract's spelling.
fn fn_snippet(name: &str, params: &[Type], ret: &Type, variadic: bool) -> String {
    if variadic {
        // An open rule has no name to complete and is never offered.
        return format!("export fn ${{1:name}}() -> {ret} {{\n    return $0\n}}");
    }
    let mut tab = 1;
    let mut ps = String::new();
    for (i, p) in params.iter().enumerate() {
        if i > 0 {
            ps.push_str(", ");
        }
        let hint = param_hint(p);
        ps.push_str(&format!("${{{tab}:{hint}}}: "));
        tab += 1;
        ps.push_str(&format!("${{{tab}:{p}}}"));
        tab += 1;
    }
    if *ret == Type::Unit {
        format!("export fn {name}({ps}) {{\n    $0\n}}")
    } else {
        format!("export fn {name}({ps}) -> {ret} {{\n    return $0\n}}")
    }
}

/// Returns a parameter name for a snippet tabstop: `T` gives `t`, any other
/// type `arg`.
fn param_hint(ty: &Type) -> String {
    match ty {
        Type::Param(p) => p.to_lowercase(),
        _ => "arg".to_string(),
    }
}

/// One offered contract member; one item per declared shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractCompletion {
    /// The member's name: what the user types.
    pub label: String,
    /// The shape as `std/contract` spells it, and its contract.
    pub detail: String,
    pub doc: Option<String>,
    pub snippet: String,
    /// Required members sort first, then declaration order. The LSP passes it on
    /// as `sortText`.
    pub sort: String,
    pub required: bool,
}

/// Returns the contract members a file's form provides without declaring
/// them: names `vyrn why --contract` must not report absent and
/// [`contract_completions`] must not offer.
///
/// A `.vyx`'s `<template>` is its view: `std/vyx` compiles it into an
/// `Html`-returning export that the `<script>` never mentions. The test is
/// the member's return type, not its name, because the consuming generator
/// chooses the name.
pub fn synthesized_members(view: &ContractView, path: &str, file_text: &str) -> Vec<String> {
    if !path.ends_with(".vyx") || !has_template(file_text) {
        return Vec::new();
    }
    view.members
        .iter()
        .filter(|m| {
            !m.shapes.is_empty() && m.shapes.iter().all(|s| s.kind == "fn" && s.ret == "Html")
        })
        .map(|m| m.name.clone())
        .collect()
}

/// Returns whether a `.vyx` source has a `<template>` section outside the
/// `<script>`, on either side of it, as `std/vyx`'s `vyxSectionAvoid` allows.
/// [`crate::vyx::script_body`] finds the script by `std/vyx`'s rule.
fn has_template(text: &str) -> bool {
    let (before, after) = match crate::vyx::script_body(text) {
        // `text[..start]` ends with the open tag, which holds no `<template`.
        Some((start, end)) => (&text[..start], &text[end + "</script>".len()..]),
        None => ("", text),
    };
    before.contains("<template") || after.contains("<template")
}

/// Returns the contract members to offer at module scope, required first.
///
/// A name in `already`, the module's exports, is not offered again. A member
/// with several shapes stays offered until one is written, because the shapes
/// are alternatives. An open rule contributes nothing: it has no name.
pub fn contract_completions(view: &ContractView, already: &[String]) -> Vec<ContractCompletion> {
    let mut out = Vec::new();
    let mut rank = 0usize;
    for required_pass in [true, false] {
        for m in &view.members {
            if m.optional == required_pass {
                continue;
            }
            if already.iter().any(|n| n == &m.name) {
                continue;
            }
            for shape in &m.shapes {
                out.push(ContractCompletion {
                    label: m.name.clone(),
                    detail: format!("{} — {}", shape.spelling, view.site()),
                    doc: m.doc.clone(),
                    snippet: shape.snippet.clone(),
                    sort: format!("{:04}", rank),
                    required: !m.optional,
                });
                rank += 1;
            }
        }
    }
    out
}

/// Returns hover markdown for a contract member: every shape, the doc, and
/// the contract. `None` when `name` is not a member.
pub fn contract_member_hover(view: &ContractView, name: &str) -> Option<String> {
    let m = view.member(name)?;
    let mut out = String::from("```vyrn\n");
    for shape in &m.shapes {
        out.push_str(&format!("{} {}: {}\n", shape.kind, m.name, shape.spelling));
    }
    out.push_str("```\n");
    if let Some(doc) = &m.doc {
        out.push_str(doc);
        if !doc.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str(&format!(
        "member of {}{}",
        view.site(),
        if m.optional {
            " — optional"
        } else {
            " — required"
        }
    ));
    Some(out)
}

/// A rename that makes an export satisfy a contract member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractFix {
    /// The export as written (`laod`).
    pub from: String,
    /// The nearby member (`data`).
    pub to: String,
    /// The export name's position in the module.
    pub line: usize,
    pub col: usize,
    pub end_col: usize,
}

/// The Damerau-Levenshtein threshold of `std/contract:nearThreshold`, so the
/// editor and the generator ask the same question.
pub const NEAR_THRESHOLD: usize = 2;

/// Returns every export a closed contract does not name that is within
/// [`NEAR_THRESHOLD`] of a member, with the position of the rename. An open
/// contract yields nothing: any name is legal in its open slot.
pub fn contract_fixes(view: &ContractView, module_source: &str) -> Vec<ContractFix> {
    if view.open_rule.is_some() {
        return Vec::new();
    }
    let Ok(tokens) = crate::lexer::lex(module_source) else {
        return Vec::new();
    };
    let cols = name_columns(module_source);
    let (program, _) = crate::parser::parse_accum(tokens);
    let mut out = Vec::new();
    for f in &program.functions {
        if !f.exported || view.member(&f.name).is_some() {
            continue;
        }
        let Some(near) = did_you_mean(view, &f.name) else {
            continue;
        };
        let (col, end_col) = cols
            .get(&(f.line, f.name.clone()))
            .copied()
            .unwrap_or((0, 0));
        out.push(ContractFix {
            from: f.name.clone(),
            to: near,
            line: f.line,
            col,
            end_col,
        });
    }
    out
}

/// Returns the member nearest `name` within [`NEAR_THRESHOLD`]; ties go to
/// declaration order, as `std/contract:didYouMean`.
fn did_you_mean(view: &ContractView, name: &str) -> Option<String> {
    let mut best: Option<(usize, &str)> = None;
    for m in &view.members {
        let d = edit_distance(name, &m.name);
        if best.map(|(bd, _)| d < bd).unwrap_or(true) {
            best = Some((d, &m.name));
        }
    }
    best.filter(|(d, _)| *d <= NEAR_THRESHOLD)
        .map(|(_, n)| n.to_string())
}

/// Damerau-Levenshtein distance (optimal string alignment) in bytes, the twin
/// of `std/strings:editDistance`. The checker asks it on every `vyrn check` and
/// the editor on every code action, and neither runs compiled Vyrn; a test over
/// the same cases pins the two together.
pub fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (n, m) = (a.len(), b.len());
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for j in 0..=m {
        d[0][j] = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = best;
        }
    }
    d[n][m]
}
