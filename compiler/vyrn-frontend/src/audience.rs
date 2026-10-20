//! Audience: who runs a module, read off the module's own path.
//!
//! `server/store.vyrn` is server-only, `app/routes/index.vyx` universal and
//! `client/boot.vyrn` client-only. A project declares its vocabulary once in
//! `vyrn.json`:
//!
//! ```json
//! { "audience": { "server": ["server"], "client": ["client"],
//!                 "universal": ["app", "shared"] } }
//! ```
//!
//! Without an `audience` key, [`from_manifest`] returns `None`: every module
//! is universal and no import is refused. The last audience segment on the
//! path decides, so `server/api/pastes.vyrn` and
//! `src/pastes/server/api/pastes.vyrn` agree; [`crate::contracts::role_for`]
//! scores role scopes the same way, so the two path axes compose.

use crate::schema::Json;
use crate::session::Session;

/// Who runs a module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience {
    /// Server-only: never in the client bundle.
    Server,
    /// Client-only: never in the server binary.
    Client,
    /// Both the SSR and the client bundle, or no UI: the default, legal to import
    /// from anywhere.
    Universal,
}

impl Audience {
    /// The diagnostic's name for it (`server-only`, `universal`).
    pub fn phrase(self) -> &'static str {
        match self {
            Audience::Server => "server-only",
            Audience::Client => "client-only",
            Audience::Universal => "universal",
        }
    }

    /// The `vyrn.json` key that declares this audience's segments.
    pub fn key(self) -> &'static str {
        match self {
            Audience::Server => "audience.server",
            Audience::Client => "audience.client",
            Audience::Universal => "audience.universal",
        }
    }
}

impl std::fmt::Display for Audience {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Audience::Server => "server",
            Audience::Client => "client",
            Audience::Universal => "universal",
        })
    }
}

/// Maps a path to file identity (`std::fs::canonicalize` in every consumer
/// with a filesystem); `None` for a remote key, an in-memory module or a
/// missing file. The frontend never touches the disk, so the CLI and the LSP
/// hand their maps the same function (`vyrn_cli`'s `real_path`).
pub type RealPath = fn(&str) -> Option<String>;

/// A project's declared audience vocabulary, rooted at a directory.
///
/// `base` is the manifest's directory: only paths under it obey the rule, so a
/// `std/` file under some ancestor named `client/` has no audience.
#[derive(Debug, Clone, Default)]
pub struct AudienceMap {
    pub server: Vec<String>,
    pub client: Vec<String>,
    pub universal: Vec<String>,
    /// The project's entry points and the audience each has as one:
    /// `(slash path, audience, manifest key)`. A composition root names both
    /// sides and no path segment can say so, but the manifest's `server`,
    /// `client` and `main` keys already name it.
    pub entries: Vec<(String, Audience, String)>,
    /// Slash-separated project directory. Empty means every path obeys the rule.
    pub base: String,
    /// The consumer's file-identity function, or `None` to compare paths as
    /// written. Audience belongs to a file, not a spelling, so every decision
    /// goes through [`AudienceMap::identity`] first.
    pub realpath: Option<RealPath>,
}

/// Two maps are equal when they declare the same thing; `realpath` is the
/// consumer's reading of the disk, not part of the declaration.
impl PartialEq for AudienceMap {
    fn eq(&self, other: &Self) -> bool {
        self.server == other.server
            && self.client == other.client
            && self.universal == other.universal
            && self.entries == other.entries
            && self.base == other.base
    }
}

impl Eq for AudienceMap {}

impl AudienceMap {
    /// Decides on file identity as `f` reports it. The base and entry points are
    /// converted too, or nothing would match the converted keys.
    pub fn with_realpath(mut self, f: RealPath) -> Self {
        self.base = f(&self.base).unwrap_or(self.base);
        for (path, _, _) in self.entries.iter_mut() {
            *path = f(path).unwrap_or_else(|| path.clone());
        }
        self.realpath = Some(f);
        self
    }

    /// Returns `path` as file identity: what the filesystem calls it, or `path`
    /// when nothing on disk answers. A `session` answers from its memo of
    /// [`crate::manifest::real_path`], the `realpath` every manifest's map holds.
    fn identity(&self, path: &str, session: Option<&Session>) -> String {
        let real = match (self.realpath, session) {
            (None, _) => None,
            (Some(_), Some(s)) => s.real_path(path),
            (Some(f), None) => f(path),
        };
        real.unwrap_or_else(|| path.to_string())
    }
}

/// What decided a module's audience.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// The nearest audience segment on its path.
    Segment(String),
    /// The `vyrn.json` key naming it as an entry point (`server`, `client`,
    /// `main`).
    Entry(String),
    /// Nothing did: universal is the default.
    Default,
}

/// One resolved audience, with what decided it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub audience: Audience,
    pub reason: Reason,
}

impl Verdict {
    /// The universal default.
    pub fn universal() -> Self {
        Verdict {
            audience: Audience::Universal,
            reason: Reason::Default,
        }
    }

    /// Returns the "why" of every diagnostic and of `vyrn why`, such as
    /// `path segment `server` (vyrn.json audience.server)`.
    pub fn because(&self) -> String {
        match &self.reason {
            Reason::Segment(s) => {
                format!("path segment `{s}` (vyrn.json {})", self.audience.key())
            }
            Reason::Entry(k) => {
                format!("being this project's `{k}` entry point (vyrn.json:{k})")
            }
            Reason::Default => {
                "no audience segment on its path (universal is the default)".to_string()
            }
        }
    }
}

/// Returns the `"audience"` map of a `vyrn.json` document, or `None` when it
/// has no such key.
///
/// `None` keeps a project that has not opted in unchecked. It takes the parsed
/// document, so a manifest that fails to parse cannot read as "no audience";
/// refusing it is the file reader's job.
pub fn from_manifest(doc: &Json, base: &str) -> Option<AudienceMap> {
    let Some(Json::Obj(entries)) = doc.get("audience") else {
        return None;
    };
    let list = |key: &str| -> Vec<String> {
        match entries.iter().find(|(k, _)| k == key).map(|(_, v)| v) {
            Some(Json::Arr(items)) => items
                .iter()
                .filter_map(|i| match i {
                    Json::Str(s) => Some(s.clone()),
                    _ => None,
                })
                .collect(),
            Some(Json::Str(s)) => vec![s.clone()],
            _ => Vec::new(),
        }
    };
    let base = base.replace('\\', "/").trim_end_matches('/').to_string();
    // Entry points come from the manifest's top-level keys. `main` runs on the
    // machine it was built for, so it is server-side: it may reach server
    // modules and not client-only ones.
    let mut entries = Vec::new();
    for (key, audience) in [
        ("server", Audience::Server),
        ("client", Audience::Client),
        ("main", Audience::Server),
    ] {
        if let Some(Json::Str(rel)) = doc.get(key) {
            let path = if base.is_empty() {
                rel.clone()
            } else {
                format!("{base}/{rel}")
            };
            entries.push((crate::loader::normalize(&path), audience, key.to_string()));
        }
    }
    Some(AudienceMap {
        server: list("server"),
        client: list("client"),
        universal: list("universal"),
        entries,
        base,
        realpath: None,
    })
}

/// Returns the audience of the module key `path` and what decided it.
///
/// A generated module's banner key resolves to the file a person wrote, so a
/// page's generated modules inherit the page's audience. A `session` keeps the
/// file identities it asks the disk for.
pub fn audience_of(key: &str, map: &AudienceMap, session: Option<&Session>) -> Verdict {
    let verdict = declared_audience_of(key, map, session);
    if verdict.audience != Audience::Universal {
        return verdict;
    }
    // A universal generated module takes the audience of the root that mounts
    // it. `vyxPage` and `vyxPageClient` compile one `.vyx` into modules for
    // opposite sides of the wire; the SSR half must reach the server, and the
    // client half is checked against the client root.
    if let Some(importer) = crate::loader::generated_importer(key) {
        let caller = audience_of(importer, map, session);
        if caller.audience != Audience::Universal {
            return caller;
        }
    }
    verdict
}

/// Returns the audience `path` declares: the manifest key naming it as an
/// entry point, else the nearest audience segment.
fn declared_audience_of(path: &str, map: &AudienceMap, session: Option<&Session>) -> Verdict {
    let path = map.identity(&source_file(path), session);
    // An entry point's audience is declared by its key, so it beats the path.
    if let Some((_, a, key)) = map
        .entries
        .iter()
        .find(|(p, _, _)| same_path(p, &path, &map.base))
    {
        return Verdict {
            audience: *a,
            reason: Reason::Entry(key.clone()),
        };
    }
    let rel = match relative_to(&path, &map.base) {
        Some(r) => r,
        // Outside the project (std, a remote or vendored module): universal.
        None => return Verdict::universal(),
    };
    // Directory components only: a file named `server.vyrn` is a composition
    // root, not a server-only module.
    let mut comps: Vec<&str> = rel.split('/').collect();
    comps.pop();
    let mut out = Verdict::universal();
    for c in comps {
        if let Some(a) = classify(c, map) {
            out = Verdict {
                audience: a,
                reason: Reason::Segment(c.to_string()),
            };
        }
    }
    out
}

/// Returns whether two slash paths name the same module. An entry point is
/// relative to the manifest while a module key may be relative to the working
/// directory, so both are read against the project directory `base`, as
/// [`relative_to`] does.
pub(crate) fn same_path(a: &str, b: &str, base: &str) -> bool {
    if a == b {
        return true;
    }
    // Fold `.`, `..` and a leading slash so whole paths compare whole.
    let (na, nb) = (join_normalized("", a), join_normalized("", b));
    if na == nb {
        return true;
    }
    // One spelling may carry the project directory in front of the other; strip
    // it from either side. Any other divergence is a different file that shares
    // the tail, and must not win the entry verdict (`screens/main.vyrn` is not
    // the `main` root).
    if base.is_empty() {
        return false;
    }
    let dir = join_normalized("", base);
    if dir.is_empty() {
        return false;
    }
    let d: &str = dir.as_str();
    let sa = na.strip_prefix(d).and_then(|r| r.strip_prefix('/'));
    let sb = nb.strip_prefix(d).and_then(|r| r.strip_prefix('/'));
    sa == Some(nb.as_str()) || sb == Some(na.as_str())
}

/// Returns the file a module key's audience is read from: itself, or for a
/// generated module the source a person wrote.
///
/// That is the generator's input file when it names one
/// (`vyxPage("./app/routes/index.vyx")`), because audience belongs to the
/// file. A generator pointed at a directory (`pages("./app/routes")`) makes
/// router glue, which takes the calling module's file.
fn source_file(key: &str) -> String {
    let importer = crate::loader::generated_importer(key)
        .unwrap_or(key)
        .replace('\\', "/");
    match first_generator_arg(key).and_then(|arg| generator_input(&importer, &arg)) {
        Some(input) => input,
        None => importer,
    }
}

/// Returns the one input file a generator call names, resolved against
/// `importer`, the module that wrote the call. `None` for a directory (no
/// extension on the last component), a `std/` specifier, or an importer with
/// no directory. The audience of a generated module ([`source_file`]) and the
/// edge `vyrn why` draws both ask this, and must agree.
pub fn generator_input(importer: &str, arg: &str) -> Option<String> {
    let last = arg.rsplit('/').next().unwrap_or(arg);
    if !last.contains('.') || arg.starts_with("std/") {
        return None;
    }
    let importer = importer.replace('\\', "/");
    let dir = importer.rfind('/').map(|i| importer[..i].to_string())?;
    Some(join_normalized(&dir, arg))
}

/// One generator import's claim on the files its first argument names. The
/// import graph is the only registry of generator inputs: a file is an input
/// because a claim covers it, whatever its extension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    /// The module that wrote the import, as the caller spelled it.
    pub importer: String,
    /// The import's 1-based line in `importer`.
    pub line: usize,
    /// The specifier the generator was imported from; `None` for a local `gen fn`.
    pub module: Option<String>,
    /// The generator's name as its module exports it.
    pub generator: String,
    /// The first argument as the import spells it.
    pub arg: String,
    /// The first argument resolved against `importer`: the file
    /// [`generator_input`] names, else a directory or a module path with the
    /// `.vyrn` that `resolve_spec` appends stripped.
    pub target: String,
}

impl Claim {
    /// Whether the claim covers `file`: the target, a file under it, or the
    /// module the target names.
    pub fn covers(&self, file: &str) -> bool {
        let key = crate::origin::OriginMaps::norm_path_key;
        let (t, f) = (key(&self.target), key(file));
        f == t || f == format!("{t}.vyrn") || f.starts_with(&format!("{t}/"))
    }

    /// Spells a file the claim covers as the import would: [`Self::arg`], then
    /// the file's path under the target. `None` for a file it does not cover.
    pub fn spell(&self, file: &str) -> Option<String> {
        let key = crate::origin::OriginMaps::norm_path_key;
        let (t, f) = (key(&self.target), key(file));
        if f == t {
            return Some(self.arg.clone());
        }
        let under = f.strip_prefix(&format!("{t}/"))?;
        let rest = file.get(file.len() - under.len()..)?;
        Some(format!("{}/{rest}", self.arg))
    }
}

/// Returns the claims of `program`'s generator imports, in source order. An
/// import whose first argument is not a string literal, or does not resolve,
/// claims nothing.
pub fn claims(
    importer: &str,
    program: &crate::ast::Program,
    opts: &crate::loader::LoadOptions,
) -> Vec<Claim> {
    use crate::ast::{Expr, ImportSource};
    let module_of = |name: &str| {
        program.imports.iter().find_map(|imp| match &imp.source {
            ImportSource::Path(spec) => (imp.names.iter())
                .find(|n| n.local() == name)
                .map(|n| (spec.clone(), n.original.clone())),
            _ => None,
        })
    };
    let mut out = Vec::new();
    for imp in &program.imports {
        let ImportSource::Generator { name, args, .. } = &imp.source else {
            continue;
        };
        let Some(Expr::Str(arg, _)) = args.first() else {
            continue;
        };
        let target = match generator_input(importer, arg) {
            Some(file) => file,
            None => match crate::loader::resolve_spec(arg, importer, opts) {
                Ok(r) => r.strip_suffix(".vyrn").unwrap_or(&r).to_string(),
                Err(_) => continue,
            },
        };
        let (module, generator) = match module_of(name) {
            Some((spec, original)) => (Some(spec), original),
            None => (None, name.clone()),
        };
        out.push(Claim {
            importer: importer.to_string(),
            line: imp.line,
            module,
            generator,
            arg: arg.clone(),
            target,
        });
    }
    out
}

/// The Vyrn a tool reads in a generator input, as the editor companion of the
/// claiming generator module writes it through `gen fn vyrnSource*(path:
/// String) -> String`: the input with every character outside Vyrn code
/// replaced by a space and each LF kept, then declarations the file's form
/// provides. Positions in `code` are the
/// input's, so a tool maps none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputView {
    /// The view's first lines, one per line of the input.
    pub code: String,
    /// Per input line, whether it is Vyrn code. A line with a character the view
    /// keeps is; a line the view blanks is not; an empty line takes the verdict
    /// of the nearest line above or below that has one, so a blank line inside
    /// a script is code and one between two template lines is not.
    pub code_lines: Vec<bool>,
    /// The names the declarations after the input's last line export: members
    /// the file's form writes, such as a view its template compiles to.
    pub provided: Vec<String>,
}

impl InputView {
    /// Splits the generator's `view` of an input whose text is `input`.
    pub fn new(input: &str, view: &str) -> InputView {
        let n = input.lines().count();
        let lines: Vec<&str> = view.lines().collect();
        let code = lines[..n.min(lines.len())].join("\n");
        let tail = lines.get(n..).unwrap_or_default().join("\n");
        let provided = match crate::lexer::lex(&tail) {
            Ok(tokens) => (crate::parser::parse_accum(tokens).0.functions.into_iter())
                .filter(|f| f.exported)
                .map(|f| f.name)
                .collect(),
            Err(_) => Vec::new(),
        };
        let kept: Vec<Option<bool>> =
            (input.lines().zip(code.lines().chain(std::iter::repeat(""))))
                .map(|(i, v)| (!i.trim().is_empty()).then(|| !v.trim().is_empty()))
                .collect();
        let near = |k: usize| {
            let above = kept[..k].iter().rev().find_map(|v| *v);
            let below = kept[k..].iter().find_map(|v| *v);
            above == Some(true) || below == Some(true)
        };
        let code_lines = (0..kept.len())
            .map(|k| kept[k].unwrap_or_else(|| near(k)))
            .collect();
        InputView {
            code,
            code_lines,
            provided,
        }
    }

    /// Whether 1-based `line` of the input is Vyrn code.
    pub fn is_code(&self, line: usize) -> bool {
        line.checked_sub(1).and_then(|l| self.code_lines.get(l)) == Some(&true)
    }
}

/// Returns the view of the input `file` that `claim` covers, through the
/// `gen fn vyrnSource*` of its generator module's editor companion
/// ([`crate::loader::call_generator`]). `None` when the claim names no module,
/// no companion declares such a function, the file cannot be read, or the run
/// fails.
pub fn input_view(
    claim: &Claim,
    file: &str,
    opts: &crate::loader::LoadOptions,
    resolver: &dyn crate::loader::ModuleResolver,
    engine: Option<&crate::gen::GenEngine>,
) -> Option<InputView> {
    let input = resolver.read(file).ok()?;
    let arg = crate::consteval::ConstVal::Str(claim.spell(file)?);
    let module = claim.module.as_deref()?;
    let view = crate::loader::call_generator(
        &claim.importer,
        module,
        "vyrnSource",
        &[arg],
        true,
        opts,
        resolver,
        engine,
    )?
    .ok()?;
    Some(InputView::new(&input, &view))
}

/// Returns the definition the editor companion of `claim`'s generator module
/// gives for 1-based `line` and `col` of the input `file`, through its
/// `gen fn vyrnDefinition*(root, path, line, col) -> String`
/// ([`crate::loader::call_generator`]). The answer is an anchor in `//@origin`
/// notation, resolved as an origin path is. `None` when the claim names no
/// module, no companion declares such a function, the run fails, or the answer
/// is empty or malformed.
#[allow(clippy::too_many_arguments)]
pub fn input_definition(
    claim: &Claim,
    file: &str,
    line: usize,
    col: usize,
    opts: &crate::loader::LoadOptions,
    resolver: &dyn crate::loader::ModuleResolver,
    engine: Option<&crate::gen::GenEngine>,
) -> Option<crate::origin::Origin> {
    use crate::consteval::ConstVal;
    let args = [
        ConstVal::Str(claim.arg.clone()),
        ConstVal::Str(claim.spell(file)?),
        ConstVal::Int(line as i64),
        ConstVal::Int(col as i64),
    ];
    let module = claim.module.as_deref()?;
    let out = crate::loader::call_generator(
        &claim.importer,
        module,
        "vyrnDefinition",
        &args,
        false,
        opts,
        resolver,
        engine,
    )?
    .ok()?;
    let dir = claim.importer.rsplit_once('/').map_or("", |(d, _)| d);
    let project = if opts.alias_base.is_empty() {
        dir
    } else {
        opts.alias_base.as_str()
    };
    let ctx = crate::origin::Context::new(None, dir, project);
    crate::origin::anchor(out.trim(), &ctx).ok()
}

/// Returns the first string argument of the innermost generator call in a
/// banner key (`generated by vyxPage("./x.vyx") at ...`).
fn first_generator_arg(key: &str) -> Option<String> {
    let rest = key.strip_prefix("generated by ")?;
    let open = rest.find('(')?;
    let after = &rest[open + 1..];
    let q = after.find('"')?;
    let tail = &after[q + 1..];
    let end = tail.find('"')?;
    Some(tail[..end].to_string())
}

/// Joins `dir` and `rel`, folding `.` and `..` components.
fn join_normalized(dir: &str, rel: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for c in dir.split('/').chain(rel.split('/')) {
        match c {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    let joined = out.join("/");
    if dir.starts_with('/') {
        format!("/{joined}")
    } else {
        joined
    }
}

/// Returns the audience `segment` names. A segment listed under several keys
/// takes the first in server, client, universal order.
fn classify(segment: &str, map: &AudienceMap) -> Option<Audience> {
    if map.server.iter().any(|s| s == segment) {
        return Some(Audience::Server);
    }
    if map.client.iter().any(|s| s == segment) {
        return Some(Audience::Client);
    }
    if map.universal.iter().any(|s| s == segment) {
        return Some(Audience::Universal);
    }
    None
}

/// Returns `path` with `base` stripped, or `None` when it is not under `base`.
/// An empty base matches everything.
///
/// A module key is as relative as the path the CLI was handed, while the
/// manifest directory may be absolute. A relative key against an absolute base
/// is the same project spelled from inside it, so it is taken as
/// project-relative.
pub(crate) fn relative_to(path: &str, base: &str) -> Option<String> {
    if base.is_empty() {
        return Some(path.trim_start_matches('/').to_string());
    }
    if let Some(rest) = path.strip_prefix(base) {
        if rest.is_empty() {
            return None;
        }
        return rest.strip_prefix('/').map(|r| r.to_string());
    }
    if is_absolute(base) && !is_absolute(path) {
        return Some(path.to_string());
    }
    None
}

/// Returns whether a slash path is rooted or drive-qualified.
pub(crate) fn is_absolute(path: &str) -> bool {
    path.starts_with('/') || {
        let b = path.as_bytes();
        b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic()
    }
}

/// Returns whether `importer` importing `imported` widens audience, the one
/// illegal edge.
///
/// Anything may import a universal module; server imports server and client
/// imports client. Any other edge reaching a server-only or client-only module
/// is illegal.
pub fn widens(importer: Audience, imported: Audience) -> bool {
    match imported {
        Audience::Universal => false,
        other => other != importer,
    }
}

/// Returns the advice a refused import ends with, naming the module the edge
/// reached: a server module through [`crate::floor::crossing`], the line the
/// floor's diagnostic also ends with, and a client module by the module to
/// split.
pub fn remedy(imported: Audience, importer: &str, module: &str, map: &AudienceMap) -> String {
    match imported {
        Audience::Server => format!(
            "call it through `{}` instead",
            crate::floor::crossing(importer, module)
        ),
        Audience::Client => format!(
            "move the shared part of `{}` into a universal module and import that instead",
            display_path(module, map)
        ),
        Audience::Universal => String::new(),
    }
}

/// Returns `path` as a project reader types it: relative to the project
/// directory when inside it. An absolute temp path in a diagnostic is noise.
pub fn display_path(path: &str, map: &AudienceMap) -> String {
    let path = map.identity(&source_file(path), None);
    relative_to(&path, &map.base).unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generator_import_claims_what_its_first_argument_names() {
        let src = "import { pages } from \"std/ui\"\n\
                   import { route } from pages(\"./routes\")\n\
                   import { f } from table(\"./users.tbl\")\n";
        let program = crate::parser::parse_accum(crate::lexer::lex(src).unwrap()).0;
        let cs = claims("/p/app.vyrn", &program, &Default::default());
        assert_eq!(cs.len(), 2, "{cs:?}");
        assert_eq!((cs[0].line, cs[0].module.as_deref()), (2, Some("std/ui")));
        assert!(cs[0].covers("/p/routes/blog/index.vyx"));
        assert!(!cs[0].covers("/p/routesx/index.vyx"));
        assert_eq!(cs[1].module, None);
        assert!(cs[1].covers("/p/users.tbl"));
        assert_eq!(
            cs[0].spell("/p/routes/Blog/a.vyx").as_deref(),
            Some("./routes/Blog/a.vyx")
        );
        assert_eq!(cs[1].spell("/p/users.tbl").as_deref(), Some("./users.tbl"));
        assert!(!cs[1].covers("/p/users.tblx"));
    }

    fn map() -> AudienceMap {
        AudienceMap {
            server: vec!["server".into()],
            client: vec!["client".into()],
            universal: vec!["app".into(), "shared".into()],
            entries: Vec::new(),
            base: "/p".into(),
            realpath: None,
        }
    }

    /// Parses `json` and reads its audience map.
    fn from_text(json: &str, base: &str) -> Option<AudienceMap> {
        from_manifest(&crate::schema::parse_json(json).unwrap(), base)
    }

    #[test]
    fn no_audience_key_is_no_map() {
        assert!(from_text("{\"name\":\"x\"}", "/p").is_none());
    }

    #[test]
    fn the_documented_manifest_shape() {
        let m = from_text(
            r#"{"audience":{"server":["server"],"client":["client"],"universal":["app","shared"]}}"#,
            "/p/",
        )
        .unwrap();
        assert_eq!(m.server, vec!["server".to_string()]);
        assert_eq!(m.universal, vec!["app".to_string(), "shared".to_string()]);
        assert_eq!(m.base, "/p");
    }

    #[test]
    fn audience_outer_and_feature_outer_agree() {
        let m = map();
        assert_eq!(
            audience_of("/p/server/api/pastes.vyrn", &m, None).audience,
            Audience::Server
        );
        assert_eq!(
            audience_of("/p/src/pastes/server/api/pastes.vyrn", &m, None).audience,
            Audience::Server
        );
    }

    #[test]
    fn nearest_segment_wins() {
        let m = map();
        // A universal directory under a server one is universal.
        let v = audience_of("/p/server/app/widget.vyrn", &m, None);
        assert_eq!(v.audience, Audience::Universal);
        assert_eq!(v.reason, Reason::Segment("app".into()));
        // And the other way round.
        let v = audience_of("/p/app/server/secret.vyrn", &m, None);
        assert_eq!(v.audience, Audience::Server);
        assert_eq!(v.reason, Reason::Segment("server".into()));
    }

    #[test]
    fn a_file_named_server_is_not_a_server_module_unless_the_manifest_says_so() {
        let m = map();
        let v = audience_of("/p/server.vyrn", &m, None);
        assert_eq!(v.audience, Audience::Universal);
        assert_eq!(v.reason, Reason::Default);

        // But the manifest naming it as the server entry point does say so.
        let m = from_text(
            r#"{"server":"server.vyrn","client":"client.vyrn",
                "audience":{"server":["server"],"client":["client"],"universal":["app"]}}"#,
            "/p",
        )
        .unwrap();
        let v = audience_of("/p/server.vyrn", &m, None);
        assert_eq!(v.audience, Audience::Server);
        assert_eq!(v.reason, Reason::Entry("server".into()));
        assert_eq!(
            audience_of("/p/client.vyrn", &m, None).audience,
            Audience::Client
        );
    }

    #[test]
    fn outside_the_project_has_no_audience() {
        let m = map();
        assert_eq!(
            audience_of("/elsewhere/server/x.vyrn", &m, None).audience,
            Audience::Universal
        );
    }

    /// Two spellings of one file get one audience.
    #[test]
    fn a_second_spelling_of_one_file_has_one_audience() {
        // Stands in for the filesystem: `Server` and the junction `vendor` resolve to
        // the real `server`.
        fn realpath(p: &str) -> Option<String> {
            Some(
                p.replace("/Server/", "/server/")
                    .replace("/vendor/", "/server/"),
            )
        }
        let m = map().with_realpath(realpath);
        for spelling in [
            "/p/server/store.vyrn",
            "/p/Server/store.vyrn",
            "/p/vendor/store.vyrn",
        ] {
            assert_eq!(
                audience_of(spelling, &m, None).audience,
                Audience::Server,
                "{spelling}"
            );
            // Every consumer names the file, not the spelling.
            assert_eq!(display_path(spelling, &m), "server/store.vyrn");
        }
    }

    /// The generated module's audience and `vyrn why`'s edge ask one function.
    #[test]
    fn a_generator_argument_names_one_input_file_or_a_directory() {
        // One file: a `.vyx` mounted per page.
        assert_eq!(
            generator_input("/p/client/boot.vyrn", "../server/pages/Leak.vyx").as_deref(),
            Some("/p/server/pages/Leak.vyx")
        );
        // A directory has no single input; its module takes its caller's.
        assert_eq!(
            generator_input("/p/client/boot.vyrn", "../app/widgets"),
            None
        );
        assert_eq!(generator_input("/p/main.vyrn", "std/rpc"), None);
        // And the generated module's audience reads exactly that.
        let m = map();
        let banner = "generated by vyxPage(\"../server/pages/Leak.vyx\") at /p/client/boot.vyrn";
        assert_eq!(audience_of(banner, &m, None).audience, Audience::Server);
        let glue = "generated by components(\"../app/widgets\") at /p/client/boot.vyrn";
        assert_eq!(audience_of(glue, &m, None).audience, Audience::Client);
    }

    #[test]
    fn the_legal_edges() {
        use Audience::*;
        assert!(!widens(Universal, Universal));
        assert!(!widens(Server, Universal));
        assert!(!widens(Client, Universal));
        assert!(!widens(Server, Server));
        assert!(!widens(Client, Client));
        assert!(widens(Universal, Server));
        assert!(widens(Universal, Client));
        assert!(widens(Client, Server));
        assert!(widens(Server, Client));
    }

    /// A deeper file ending in an entry's path is not that entry point, so it
    /// gets no entry verdict to exempt it from the cross-audience check.
    #[test]
    fn a_file_sharing_an_entrys_tail_is_not_that_entry_point() {
        let m = from_text(
            r#"{"main":"main.vyrn","audience":{"server":["server"]}}"#,
            "",
        )
        .unwrap();
        let v = audience_of("/p/screens/main.vyrn", &m, None);
        assert_eq!(v.audience, Audience::Universal);
        assert_eq!(v.reason, Reason::Default);
    }
}
