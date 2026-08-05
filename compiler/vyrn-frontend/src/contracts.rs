//! RFC-0071 M4 — the editor's view of a module contract.
//!
//! M1 made `contract` a declaration and M2 made `head`/`data` members of one.
//! What makes a declared convention better than a scanned one is that you can
//! SEE it, and seeing it is this module's job: role → contract resolution, then
//! completion / hover / go-to-definition / did-you-mean over the resolved
//! declaration.
//!
//! Everything here is a query over an ordinary [`crate::ast::ContractDecl`]. The
//! compiler still knows nothing about `Page` or `Component` in particular — a
//! third-party generator that declares its own contract gets identical editor
//! support with no change here and none in the LSP. That is the same promise
//! `std/contract` keeps at generation time, kept a second time at edit time.
//!
//! **The LSP stays a pure adapter.** The server calls [`roles_for`],
//! [`load_contract`] and the four query functions, and maps their results onto
//! LSP shapes. No contract knowledge is compiled into it.

use crate::ast::{
    ContractDecl, ContractMember, ContractMemberKind, Expr, ImportSource, Program, Type,
};
use crate::loader::{LoadOptions, ModuleResolver};
use crate::schema::Json;

// ---------------------------------------------------------------------------
// Roles: which contract governs which file
// ---------------------------------------------------------------------------

/// Where a role applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoleScope {
    /// A path SEGMENT, as `vyrn.json`'s `roles` map declares it: every module
    /// under a directory with this name is in the role. This is the form
    /// RFC-0071 specifies and RFC-0072 inherits.
    ///
    /// RFC-0072 M2 lets it be a RUN of segments (`"server/api"`), matched
    /// consecutively. Audience introduced a second axis of path segments, and a
    /// single-component scope can only pick one of the two: `api/` under
    /// `server/` and `api/` under `client/` would be the same role, whatever a
    /// project meant by them. A run composes the axes instead — the audience
    /// segment and the role segment in one scope — and the plain one-segment
    /// form is the degenerate case, unchanged.
    Segment(String),
    /// A resolved DIRECTORY. Produced by the fallback discovery below, which
    /// reads the directory a generator was actually pointed at
    /// (`pages("./routes")`) rather than a name anyone blessed.
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
    /// The contract's module, as a reader would type it (`std/ui`, `./gen`).
    pub module: String,
    /// The module's resolved file, when the role's source already knew it —
    /// discovery resolves the generator's own import, so a relative specifier
    /// like `./gen` is not re-resolved against a page that lives two
    /// directories away. `None` for a manifest role, whose relative specifiers
    /// are resolved against the manifest.
    pub module_file: Option<String>,
    /// The contract's name (`Page`).
    pub contract: String,
    /// File STEMS in scope that are not modules of this role. A `routes/`
    /// directory holds pages, but also `layout.vyx` and `error.vyx`, which are
    /// chrome: they have no contract to be a member of, so offering `head`/`data`
    /// inside one would be exactly the misfire this milestone exists to prevent.
    ///
    /// The default mirrors `std/ui`'s own `uiScanAll` chrome test. It is the one
    /// blessed-name table in this module, it is overridable per role from
    /// `vyrn.json`, and a `Layout` contract (not in RFC-0071) would delete it.
    pub except: Vec<String>,
}

/// The stems the fallback excludes — `std/ui`'s chrome (`layout.vyx`,
/// `error.vyx`). See [`Role::except`].
pub const DEFAULT_ROLE_EXCEPT: &[&str] = &["layout", "error"];

/// `spec` split as `module:Contract` (`"std/ui:Page"`). `None` when it has no
/// `:` — the mapping is data, so a malformed entry is simply ignored rather than
/// diagnosed (the generator's own check is where a bad contract is reported).
fn split_spec(spec: &str) -> Option<(String, String)> {
    let (m, c) = spec.rsplit_once(':')?;
    if m.is_empty() || c.is_empty() {
        return None;
    }
    Some((m.to_string(), c.to_string()))
}

/// The roles declared in a `vyrn.json` document's `"roles"` key (RFC-0071):
///
/// ```json
/// { "roles": { "routes": "std/ui:Page", "widgets": "std/vyx:Component" } }
/// ```
///
/// A value may also be an object — `{ "contract": "std/ui:Page", "except":
/// ["layout", "error"] }` — which is how a project overrides the chrome stems
/// for its own directory layout. The string form is the RFC's, and takes
/// [`DEFAULT_ROLE_EXCEPT`].
///
/// Returns an empty vector when the manifest has no `roles` key, which is the
/// signal to fall back to [`discovered_roles`].
pub fn roles_from_manifest(json_text: &str) -> Vec<Role> {
    let Ok(doc) = crate::schema::parse_json(json_text) else {
        return Vec::new();
    };
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
                let Some(Json::Str(spec)) = fields.iter().find(|(k, _)| k == "contract").map(|(_, v)| v)
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

/// The roles a project declares NOWHERE — discovered from the generator call
/// sites it already has.
///
/// `vyrn.json`'s `roles` key is RFC-0071's answer and RFC-0072 owns its general
/// form; no project in this repo writes one yet. Until they do, the question
/// "which contract governs `routes/index.vyx`?" already has an answer in the
/// source: a root module says
///
/// ```vyrn
/// import { pagesThemed } from "std/ui"
/// import { handle } from pagesThemed("./routes", "./theme.json")
/// ```
///
/// — so `./routes` is the pages directory, and the contract is the one exported
/// by the generator's own module. No blessed directory names, no table of
/// generator names: the directory comes from the call and the contract comes
/// from the module the generator was imported from. A generator module that
/// exports zero or several contracts is skipped, because then there is nothing
/// unambiguous to resolve.
///
/// `roots` are `(path, source)` pairs — the modules to scan (a project's entry
/// points).
pub fn discovered_roles(
    roots: &[(String, String)],
    opts: &LoadOptions,
    resolver: &dyn ModuleResolver,
) -> Vec<Role> {
    let mut out: Vec<Role> = Vec::new();
    for (path, source) in roots {
        let Ok(tokens) = crate::lexer::lex(source) else { continue };
        let (program, _) = crate::parser::parse_accum(tokens);
        for imp in &program.imports {
            let ImportSource::Generator { name, args, .. } = &imp.source else {
                continue;
            };
            let Some(Expr::Str(dir_spec)) = args.first() else {
                continue;
            };
            // The module the generator itself came from.
            let Some(gen_module) = generator_module(&program, name) else {
                continue;
            };
            // Its single exported contract, or nothing.
            let Some((contract, gen_file)) =
                sole_exported_contract(&gen_module, path, opts, resolver)
            else {
                continue;
            };
            let Ok(dir) = crate::loader::resolve_spec(dir_spec, path, opts) else {
                continue;
            };
            // The generator's argument is a directory; `resolve_spec` appends
            // `.vyrn` to an extension-less specifier, so strip it back off.
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

/// The module specifier the generator `name` was imported from, if it was
/// imported (a locally-declared `gen fn` has none, and a contract in the root
/// module is not a role attachment).
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

/// The one contract a module exports, with the module's parsed program. `None`
/// when it exports zero or more than one — ambiguity is not resolved by
/// guessing.
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

/// Read and parse the module `spec` names, returning its program and source.
/// Deliberately parse-only: resolving a contract must not run generators or link
/// a program, so an editor keystroke stays cheap.
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

/// Whether `path`'s file stem is DOTTED — `pastes.http.vyrn`, not `pastes.vyrn`.
///
/// RFC-0074's convention: a dotted stem is a protocol PROJECTION written OVER
/// the modules beside it, and the dot is what marks it (the same suffix that
/// derives the projection's base path). Every generator that scans a role's
/// directory already skips one — `std/rpc`'s `rpcScan` tests `st.contains(".")
/// == false`, because mounting a projection's `routes()` as a procedure would
/// put `Array<Route>` on the wire.
///
/// Role attachment is by DIRECTORY, so without this a projection sits in the
/// role of the modules it projects and is graded against their contract.
/// RFC-0071 M3 recorded exactly that and judged it inert: true of the editor,
/// where `Api` names no members and there is nothing to complete or hover, and
/// false of `vyrn why --contract`, whose entire output is the claim. The rule
/// here is the generator's own question rather than a second one — a pattern,
/// as M3 said closing it would need, not another blessed stem in
/// [`DEFAULT_ROLE_EXCEPT`].
pub fn is_projection(path: &str) -> bool {
    let file = path.replace('\\', "/");
    let file = file.rsplit('/').next().unwrap_or_default();
    file.rsplit_once('.').is_some_and(|(stem, _)| stem.contains('.'))
}

/// The role governing `path`, if any. `path` is a slash-separated module path
/// (`.vyrn` or a generator input like `.vyx`).
///
/// A file is in a role when its directory is (or is under) the role's scope, and
/// its stem is not one of the role's exceptions.
///
/// **Nearest wins**, scored by the index of the scope's LAST matched component —
/// the same rule [`crate::audience`] applies to audience segments, deliberately.
/// Audience is the outer path axis and role is the inner one, and two axes read
/// off one path have to agree about what "more specific" means or a project gets
/// to discover which one happened to win. A role scope is matched at its
/// NEAREST occurrence too, so a scope that appears twice on a path resolves the
/// way a reader would expect (the one closest to the file).
/// A file whose stem is DOTTED (`pastes.http.vyrn`) is in no role, whatever
/// directory it sits in — see [`is_projection`].
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

/// The 1-based index of the LAST component of the last consecutive run of
/// `scope`'s segments inside `comps`, or `None` when the run does not occur.
/// A one-segment scope is the ordinary case; `"server/api"` matches `server`
/// immediately followed by `api`.
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

// ---------------------------------------------------------------------------
// The resolved contract
// ---------------------------------------------------------------------------

/// One declared shape of a member — a member name may carry several
/// (RFC-0071 M2b).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractShape {
    /// `"fn"` or `"let"`, matching `std/contract`'s `Export.kind`.
    pub kind: &'static str,
    /// Parameter type spellings (`fn` shapes only).
    pub params: Vec<String>,
    /// Return / value type spelling; `""` for a `Unit` return, exactly as
    /// `std/contract` spells it.
    pub ret: String,
    /// The whole shape as `std/contract` spells it: `fn(T) -> Head`.
    pub spelling: String,
    /// Whether this shape carries a default (`= noHead()`).
    pub optional: bool,
    /// `fn *(..)` — constrains the return type only.
    pub variadic: bool,
    /// 1-based declaration line inside the contract's module.
    pub line: usize,
    /// The declaration a module writes to satisfy this shape, as an LSP snippet
    /// (`$0` / `${1:name}` tabstops). The RFC's "snippet body is the full
    /// declaration, so the type is right before the user types anything".
    pub snippet: String,
}

/// One member of a resolved contract, with every shape its name is declared at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractMemberView {
    pub name: String,
    /// The member's `///` doc. A repeated name carries its doc on the first
    /// declaration; every shape shares it, because they document one member.
    pub doc: Option<String>,
    /// Whether the module may omit the export: true when ANY shape has a
    /// default, matching `std/contract`'s `nameOptional`.
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
    /// The module specifier a reader would type (`std/ui`).
    pub module: String,
    /// The declaring module's resolved file (slash path), for go-to-definition.
    pub file: String,
    /// The contract's own `///` doc.
    pub doc: Option<String>,
    /// Named members in declaration order, one entry per distinct name.
    pub members: Vec<ContractMemberView>,
    /// The open rule (`fn *(..) -> R`), when the contract has one. A contract
    /// with an open rule admits arbitrarily-named exports of that shape, so
    /// completion offers nothing for it — there is no name to complete.
    pub open_rule: Option<ContractShape>,
}

impl ContractView {
    /// The member named `name`, if the contract declares it.
    pub fn member(&self, name: &str) -> Option<&ContractMemberView> {
        self.members.iter().find(|m| m.name == name)
    }
    /// `contract `Page` (std/ui)` — the phrase every hover ends with.
    pub fn site(&self) -> String {
        if self.module.is_empty() {
            format!("contract `{}`", self.name)
        } else {
            format!("contract `{}` ({})", self.name, self.module)
        }
    }
}

/// Resolve `module_spec:contract_name` to a [`ContractView`], reading the
/// declaring module through `resolver`.
///
/// Parse-only and link-free: an editor resolves a contract on every keystroke,
/// and running the loader (let alone generators) for it would be absurd. The
/// contract declaration is complete on its own.
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

/// The contract a role names, resolved the way the role knows how.
///
/// A discovered role already resolved the generator's own import, so it carries
/// the file; a manifest role carries a specifier, which is resolved against
/// `importer` — the manifest, so `"./contracts:Screen"` in `vyrn.json` means
/// what a reader of `vyrn.json` would think it means, not something relative to
/// whichever page happens to be open.
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

/// Build the editor view of one declaration. Member name COLUMNS come from the
/// lexer, exactly as [`crate::symbols`] finds declaration columns — the AST
/// carries only a line.
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

/// `(line, identifier) → (col, end_col)` for the FIRST occurrence of each
/// identifier on each line.
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

/// The type spelling `std/contract` compares against: a `Unit` return spells
/// `""` on both sides, so it needs no special case anywhere.
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
            params: Vec::new(),
            ret: ty.to_string(),
            spelling: m.spelling(),
            optional: default.is_some(),
            variadic: false,
            line: m.line,
            snippet: format!("export let {}: {} = $0", m.name, ty),
        },
        ContractMemberKind::Fn {
            params,
            ret,
            default,
            variadic,
        } => ContractShape {
            kind: "fn",
            params: params.iter().map(|p| p.to_string()).collect(),
            ret: ret_spelling(ret),
            spelling: m.spelling(),
            optional: default.is_some(),
            variadic: *variadic,
            line: m.line,
            snippet: fn_snippet(&m.name, params, ret, *variadic),
        },
    }
}

/// The declaration a page writes to satisfy a `fn` shape, as an LSP snippet.
///
/// Parameter NAMES are not part of a contract (only arity and types are), and a
/// member's type parameters are open — a real page writes `fn head(d:
/// Array<Paste>)`, not `fn head(d: T)`. So both the name and the type of each
/// parameter are tabstops, seeded with the contract's own spelling: the shape is
/// right immediately, and tabbing through fills in what only the page knows.
fn fn_snippet(name: &str, params: &[Type], ret: &Type, variadic: bool) -> String {
    if variadic {
        // An open rule has no name to complete; a snippet for it would invent
        // one. Kept spellable for completeness, never offered.
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

/// A readable parameter name for a snippet tabstop, from the parameter's type:
/// `T` → `t`, `Array<Paste>` → `arg`. Only the head letter of a bare type
/// parameter reads as a name.
fn param_hint(ty: &Type) -> String {
    match ty {
        Type::Param(p) => p.to_lowercase(),
        _ => "arg".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Completion
// ---------------------------------------------------------------------------

/// One offered contract member (one item per declared SHAPE).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractCompletion {
    /// The member's name — what the user is typing.
    pub label: String,
    /// The shape, as `std/contract` spells it, plus where it came from.
    pub detail: String,
    /// The member's `///` doc.
    pub doc: Option<String>,
    /// The full declaration, as an LSP snippet.
    pub snippet: String,
    /// Required members sort before optional ones; within a group, declaration
    /// order. The LSP passes this through as `sortText`.
    pub sort: String,
    pub required: bool,
}

/// The contract members a file's FORM already provides, which its text does not
/// declare — the names [`contract_status`] must not report absent and
/// [`contract_completions`] must not offer.
///
/// A `.vyx`'s `<template>` IS its view. `std/vyx` compiles it into an
/// `Html`-returning export of the module the contract actually governs — `page`
/// for a `std/ui` page, the component's own name for a widget — and the
/// `<script>` every query in this module reads never mentions it. So a page with
/// a template was reported `page: absent, optional`, which is a claim about
/// something the form guarantees exists, and completion offered a declaration
/// that would have collided with the generated one.
///
/// The test is the member's RETURN TYPE rather than its name, because the name
/// is the consuming generator's to choose and a table of generator names is the
/// drift this module refuses everywhere else. What the form guarantees is that
/// the template becomes `Html`; which member that satisfies is the contract's
/// own business.
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

/// Whether a `.vyx` source has a `<template>` section, searched OUTSIDE the
/// `<script>` — a template tag inside a script string is not one. The section
/// may sit on either side of the script, as `std/vyx`'s own `vyxSectionAvoid`
/// allows.
fn has_template(text: &str) -> bool {
    let (before, after) = match (text.find("<script"), text.find("</script>")) {
        (Some(s), Some(e)) if e > s => (&text[..s], &text[e + "</script>".len()..]),
        _ => ("", text),
    };
    before.contains("<template") || after.contains("<template")
}

/// The contract members to offer at module scope, required first.
///
/// `already` are the names the module already exports — a page that has written
/// `data` does not need it offered again, and offering it would suggest a
/// duplicate declaration. A member declared at several shapes stays offered
/// until one of them is written, because the shapes are alternatives.
///
/// A contract with an open rule contributes nothing through it: the open slot's
/// names are the application's vocabulary, so there is no name to complete.
pub fn contract_completions(view: &ContractView, already: &[String]) -> Vec<ContractCompletion> {
    let mut out = Vec::new();
    let mut rank = 0usize;
    for required_pass in [true, false] {
        for m in &view.members {
            if m.optional == required_pass {
                continue; // required pass takes !optional; optional pass takes optional
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

// ---------------------------------------------------------------------------
// Hover
// ---------------------------------------------------------------------------

/// Hover markdown for a contract member: every declared shape, the member's
/// doc, and the contract it belongs to.
///
/// Returns `None` when `name` is not a member — the caller then leaves the
/// ordinary hover alone.
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
        if m.optional { " — optional" } else { " — required" }
    ));
    Some(out)
}

// ---------------------------------------------------------------------------
// Did-you-mean → a rename quick-fix
// ---------------------------------------------------------------------------

/// A rename a module's export is one edit away from satisfying.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractFix {
    /// The export as written (`laod`).
    pub from: String,
    /// The member it is near (`data`).
    pub to: String,
    /// The offending name's position in the module.
    pub line: usize,
    pub col: usize,
    pub end_col: usize,
}

/// The Damerau-Levenshtein threshold `std/contract:nearThreshold` uses. Named
/// once here so the editor's "did you mean" and the generator's are the same
/// question.
pub const NEAR_THRESHOLD: usize = 2;

/// Every export of `module_source` that a CLOSED contract does not name but is
/// within [`NEAR_THRESHOLD`] of a member — the did-you-mean cases, with the
/// position the rename edit applies at.
///
/// An OPEN contract yields nothing: any name is legal in its open slot, so a
/// near-miss there is not a mistake (RFC-0071 says exactly this, and says it is
/// correct for `Api` and would be wrong for `Page`).
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

/// The member name closest to `name` within [`NEAR_THRESHOLD`], ties going to
/// declaration order — the same rule as `std/contract:didYouMean`.
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

/// Damerau-Levenshtein distance (optimal string alignment) — the Rust twin of
/// `std/strings:editDistance`.
///
/// A second implementation is a divergence risk, and the alternative was worse:
/// the Vyrn one is comptime library code a generator calls, and reaching it from
/// the editor would mean running the interpreter on every keystroke. The two are
/// pinned together by a test over the same cases.
pub fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
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
            let mut best = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = best;
        }
    }
    d[n][m]
}

// ---------------------------------------------------------------------------
// Status: `vyrn why --contract`
// ---------------------------------------------------------------------------

/// What a contract has to say about one name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberStatus {
    /// The module exports it, matching shape `shape` (0-based, declaration
    /// order) — the same index `std/contract:matchedMember` reports.
    Satisfied { shape: usize },
    /// A required member the module does not export.
    Missing,
    /// An optional member the module does not export; the contract's default
    /// applies.
    Defaulted,
    /// A member the file's FORM provides rather than its text — a `.vyx`'s
    /// `<template>`. Neither absent nor defaulted: see [`synthesized_members`].
    Synthesized,
    /// The module exports it at a shape the contract does not declare.
    Mismatched { found: String },
    /// An export the contract does not name (closed contract), with the member
    /// it is near, if any.
    Unknown { did_you_mean: Option<String> },
    /// An export admitted by the open rule.
    OpenMatched,
    /// An export the open rule's shape rejects.
    OpenMismatched { found: String },
}

/// One line of a contract report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEntry {
    pub name: String,
    /// The declared shapes, joined — what the contract wanted.
    pub want: String,
    pub status: MemberStatus,
}

/// Every member's status plus every unrecognized export's, in the order
/// `std/contract:checkContract` reports them: members in declaration order
/// first, then the module's other exports in source order.
///
/// `synthesized` are the members the file's form writes for it — a `.vyx`'s
/// `<template>`, from [`synthesized_members`]. They are not in `module_source`
/// and reporting them absent is a falsehood, so they are checked before absence
/// is concluded and never after: a member the text DOES declare is graded on
/// what it declares.
pub fn contract_status(
    view: &ContractView,
    module_source: &str,
    synthesized: &[String],
) -> Vec<StatusEntry> {
    let Ok(tokens) = crate::lexer::lex(module_source) else {
        return Vec::new();
    };
    let (program, _) = crate::parser::parse_accum(tokens);
    let exports: Vec<ExportSig> = program
        .functions
        .iter()
        .filter(|f| f.exported)
        .map(|f| ExportSig {
            name: f.name.clone(),
            params: f.params.iter().map(|p| p.ty.to_string()).collect(),
            ret: ret_spelling(&f.ret),
        })
        .collect();
    let mut out = Vec::new();
    for m in &view.members {
        let want = m
            .shapes
            .iter()
            .map(|s| s.spelling.clone())
            .collect::<Vec<_>>()
            .join(" or ");
        let status = match exports.iter().find(|e| e.name == m.name) {
            None if synthesized.iter().any(|n| *n == m.name) => MemberStatus::Synthesized,
            None if m.optional => MemberStatus::Defaulted,
            None => MemberStatus::Missing,
            Some(e) => match m.shapes.iter().position(|s| shape_matches(s, e)) {
                Some(i) => MemberStatus::Satisfied { shape: i },
                None => MemberStatus::Mismatched {
                    found: e.spelling(),
                },
            },
        };
        out.push(StatusEntry {
            name: m.name.clone(),
            want,
            status,
        });
    }
    for e in &exports {
        if view.member(&e.name).is_some() {
            continue;
        }
        let (want, status) = match &view.open_rule {
            Some(rule) => (
                rule.spelling.clone(),
                if shape_matches(rule, e) {
                    MemberStatus::OpenMatched
                } else {
                    MemberStatus::OpenMismatched {
                        found: e.spelling(),
                    }
                },
            ),
            None => (
                String::new(),
                MemberStatus::Unknown {
                    did_you_mean: did_you_mean(view, &e.name),
                },
            ),
        };
        out.push(StatusEntry {
            name: e.name.clone(),
            want,
            status,
        });
    }
    out
}

/// A module export flattened to what a contract member is compared against —
/// `std/contract`'s `Export`, in Rust.
struct ExportSig {
    name: String,
    params: Vec<String>,
    ret: String,
}

impl ExportSig {
    fn spelling(&self) -> String {
        let mut out = format!("fn({})", self.params.join(", "));
        if !self.ret.is_empty() {
            out.push_str(&format!(" -> {}", self.ret));
        }
        out
    }
}

/// Whether an export satisfies a shape — `std/contract:matchesSignature`.
fn shape_matches(shape: &ContractShape, e: &ExportSig) -> bool {
    if shape.kind != "fn" {
        return false;
    }
    if shape.variadic {
        return type_matches(&shape.ret, &e.ret);
    }
    if shape.params.len() != e.params.len() {
        return false;
    }
    shape
        .params
        .iter()
        .zip(&e.params)
        .all(|(p, a)| type_matches(p, a))
        && type_matches(&shape.ret, &e.ret)
}

/// Whether a member's implicit type parameter — a single uppercase ASCII letter
/// optionally followed by digits. Mirrors `parser::is_member_type_param` and
/// `std/contract:isTypeParam`.
fn is_type_param(name: &str) -> bool {
    let mut cs = name.chars();
    match cs.next() {
        Some(c) if c.is_ascii_uppercase() => {}
        _ => return false,
    }
    cs.all(|c| c.is_ascii_digit())
}

/// The head name of a type spelling: `Query<T>` → `Query`.
fn head_of(spelling: &str) -> &str {
    match spelling.find('<') {
        Some(i) => spelling[..i].trim(),
        None => spelling.trim(),
    }
}

/// Whether the actual type spelling satisfies a contract member's pattern —
/// `std/contract:typeMatches`. Equality, except that a pattern head which is a
/// member type parameter matches any type.
pub fn type_matches(pattern: &str, actual: &str) -> bool {
    let ph = head_of(pattern);
    if is_type_param(ph) {
        return true;
    }
    if ph != head_of(actual) {
        return false;
    }
    let pa = split_args(pattern);
    let aa = split_args(actual);
    if pa.len() != aa.len() {
        return false;
    }
    pa.iter().zip(&aa).all(|(p, a)| type_matches(p, a))
}

/// The depth-0 generic arguments of a type spelling, owned.
fn split_args(spelling: &str) -> Vec<String> {
    let chars: Vec<char> = spelling.chars().collect();
    let Some(open) = chars.iter().position(|&c| c == '<') else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    let mut depth = 1usize;
    let mut start = open + 1;
    let mut j = open + 1;
    while j < chars.len() && depth > 0 {
        match chars[j] {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    out.push(chars[start..j].iter().collect::<String>().trim().to_string());
                }
            }
            ',' if depth == 1 => {
                out.push(chars[start..j].iter().collect::<String>().trim().to_string());
                start = j + 1;
            }
            _ => {}
        }
        j += 1;
    }
    out
}
