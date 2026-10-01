//! Cross-boundary rename. A procedure in `server/api/pastes.vyrn` is also
//! spelled `pastesCreate` by `client()`, `rpcHandlePastesCreate` by `rpc()`,
//! and re-exported by `http()`; none of those files imports it directly. The
//! symbol map names every generated symbol whose origin is the declaration, and
//! each gets a new name derived the way its generator derives it.
//!
//! Only sources are edited: generated modules regenerate on the next load, so
//! the new generated names are predicted, not observed. References are found by
//! token ([`vyrn_frontend::references_to`]) in the project's `.vyrn` files and
//! `.vyx` script bodies whose imports provably reach the declaring module. Out
//! of reach: `.vyx` template expressions (only the script is lexed; a miss is a
//! build error) and external HTTP clients of the moved wire path. A generated
//! name that cannot be derived refuses the whole rename ([`derive_generated`]).

use std::collections::HashMap;

use vyrn_frontend::ast::{Expr, ImportDecl, ImportSource};

use crate::contracts::vyx_script;
use lsp_types::{Position, PrepareRenameResponse, Range, TextEdit, Url, WorkspaceEdit};
use vyrn_frontend::symbolmap::{same_file, MappedSymbol};
use vyrn_frontend::{analyze, references, references_to, Analysis, SymbolKind};

/// The most project files a rename will read. Higher than the `.vyx` owner
/// probe's cap: a rename runs once per refactor, and a miss breaks the build.
const MAX_RENAME_FILES: usize = 512;

/// The declaration a rename request resolves to.
pub struct Target {
    /// The declaring file, as a slash path.
    pub file: String,
    pub name: String,
    pub line: usize,
    pub col: usize,
    pub end_col: usize,
}

/// The declaration under the cursor, or the reason there is nothing to rename.
///
/// Only a top-level declaration of the open document, at its own name: renaming
/// from a use would have to re-derive the declaration in every candidate file.
pub fn target_at(
    analysis: &Analysis,
    file: &str,
    line: usize,
    col: usize,
) -> Result<Target, String> {
    // An imported name or a generated stub resolves here too; refuse it by name.
    if let Some(foreign) = analysis
        .tokens
        .iter()
        .find(|t| t.line == line && col >= t.col && col < t.end_col)
    {
        if analysis
            .symbols
            .iter()
            .any(|s| s.name == foreign.text && s.file.is_some())
            && !analysis
                .symbols
                .iter()
                .any(|s| s.name == foreign.text && s.file.is_none())
        {
            return Err(format!(
                "`{}` is not declared in this file — rename the declaration it stands for, and \
                 this use follows",
                foreign.text
            ));
        }
    }
    let decl = analysis
        .symbols
        .iter()
        .find(|s| {
            s.file.is_none() && s.line == line && s.col > 0 && col >= s.col && col <= s.end_col
        })
        .ok_or_else(|| {
            "there is no declaration here to rename — put the cursor on a top-level \
             declaration's own name"
                .to_string()
        })?;
    match decl.kind {
        SymbolKind::Function | SymbolKind::Type | SymbolKind::Global => {}
        _ => {
            return Err(format!(
                "`{}` is not a renameable declaration (renaming reaches functions, types and \
                 module state)",
                decl.name
            ))
        }
    }
    Ok(Target {
        file: file.to_string(),
        name: decl.name.clone(),
        line: decl.line,
        col: decl.col,
        end_col: decl.end_col,
    })
}

/// The editor's pre-flight: the range the rename will replace, seeded with the
/// current name, so a refusal arrives before the user types a new name.
///
/// `source` is the declaring file's text, to convert char columns to UTF-16.
pub fn prepare(target: &Target, source: &str) -> PrepareRenameResponse {
    let line_text = crate::line_of_text(source, target.line - 1);
    PrepareRenameResponse::RangeWithPlaceholder {
        range: Range {
            start: Position {
                line: (target.line - 1) as u32,
                character: crate::char_col_to_utf16(line_text, target.col - 1),
            },
            end: Position {
                line: (target.line - 1) as u32,
                character: crate::char_col_to_utf16(line_text, target.end_col - 1),
            },
        },
        placeholder: target.name.clone(),
    }
}

/// Whether `name` is a bare ASCII identifier, the only thing a rename may write:
/// every edit replaces a token span as text.
pub fn valid_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `create` -> `Create`, as the generators' `capFirst`.
fn cap_first(s: &str) -> String {
    let mut cs = s.chars();
    match cs.next() {
        Some(c) => c.to_uppercase().collect::<String>() + cs.as_str(),
        None => String::new(),
    }
}

/// The name a generated symbol takes once its declaration is renamed.
///
/// Every generator that emits a map names a symbol either as the declaration or
/// as a nonempty prefix plus `capFirst` of it (`pastesCreate`,
/// `rpcHandlePastesCreate`, `PathCreate`); the prefix is kept.
///
/// `None` for any other shape, which refuses the rename.
pub fn derive_generated(generated: &str, old: &str, new: &str) -> Option<String> {
    if generated == old {
        return Some(new.to_string());
    }
    let prefix = generated.strip_suffix(&cap_first(old))?;
    if prefix.is_empty() {
        return None;
    }
    Some(format!("{prefix}{}", cap_first(new)))
}

/// One name to rewrite, and which kind of import can reach it.
///
/// Both flags can hold for one spelling (`http()` re-exports under the
/// declaration's own name); two entries would rewrite a span twice.
struct Wanted {
    old: String,
    new: String,
    /// Reached by importing the declaring module.
    direct: bool,
    /// Reached by importing a module a generator emitted.
    generated: bool,
}

/// Every name a rename has to rewrite outside the declaring file: the
/// declaration's own name, for modules that import it directly, and one derived
/// name per generated symbol standing for it.
///
/// Map entries match on the declaration's `(name, file)`, never its line: the
/// map's line is baked when the root last generated, and an edit above the
/// declaration moves the live line. A module cannot declare two same-named
/// top-level items, so the name is unambiguous.
fn wanted(target: &Target, new: &str, maps: &[MappedSymbol]) -> Result<Vec<Wanted>, String> {
    let mut out = vec![Wanted {
        old: target.name.clone(),
        new: new.to_string(),
        direct: true,
        generated: false,
    }];
    for m in maps {
        if m.decl != target.name || !same_file(&m.file, &target.file) {
            continue;
        }
        if let Some(w) = out.iter_mut().find(|w| w.old == m.name) {
            w.generated = true;
            continue;
        }
        let derived = derive_generated(&m.name, &target.name, new).ok_or_else(|| {
            format!(
                "`{}` is generated as `{}`, whose new name this rename cannot derive — \
                 rename it by hand, or the generated call sites would break silently",
                target.name, m.name
            )
        })?;
        out.push(Wanted {
            old: m.name.clone(),
            new: derived,
            direct: false,
            generated: true,
        });
    }
    Ok(out)
}

/// A candidate file: its Vyrn body, the body's line offset within the file, and
/// the file's full text, whose lines the UTF-16 conversion reads.
struct Candidate {
    uri: Url,
    body: String,
    line_offset: usize,
    file_text: String,
}

/// How one candidate file's imports can reach the declaring module.
///
/// The declaration's own name arrives only through an import that pins the
/// module exactly (`from "./pastes"`, `http("./server/pastes")`). A derived name
/// also arrives through a directory generator (`client("./server/api")`). An
/// import aimed at another module qualifies nothing.
#[derive(Default)]
struct Reaches {
    /// Namespaces of plain path imports pinning the declaring module.
    direct_ns: Vec<String>,
    /// Namespaces of generator imports pinning the declaring module exactly.
    exact_gen_ns: Vec<String>,
    /// Namespaces of generator imports covering it through a directory argument.
    dir_gen_ns: Vec<String>,
    /// Every selective binding `(local name, pins the declaring module)`.
    flats: Vec<(String, bool)>,
}

impl Reaches {
    fn is_empty(&self) -> bool {
        self.direct_ns.is_empty()
            && self.exact_gen_ns.is_empty()
            && self.dir_gen_ns.is_empty()
            && self.flats.is_empty()
    }

    /// The qualifiers under which `w` may occur in this file, and whether it may
    /// occur at all.
    fn qualifiers(&self, w: &Wanted) -> (bool, Vec<String>) {
        let mut quals = Vec::new();
        let mut allowed = false;
        if w.direct {
            // A flat selective import has no namespace; the bare-pinning rule
            // admits it.
            allowed |= !self.direct_ns.is_empty()
                || !self.exact_gen_ns.is_empty()
                || self.bare_is_ours(&w.old);
            quals.extend(self.direct_ns.iter().chain(&self.exact_gen_ns).cloned());
        }
        if w.generated {
            allowed |= !self.exact_gen_ns.is_empty() || !self.dir_gen_ns.is_empty();
            quals.extend(self.exact_gen_ns.iter().chain(&self.dir_gen_ns).cloned());
        }
        quals.sort();
        quals.dedup();
        (allowed, quals)
    }

    /// Whether a BARE occurrence of `name` in this file must be the declaration:
    /// exactly one selective import binds the spelling, and that import pins the
    /// declaring module. Otherwise the token is skipped: a missed rename fails
    /// the build, a wrong one rewires another module's procedure silently.
    fn bare_is_ours(&self, name: &str) -> bool {
        let suppliers = self.flats.iter().filter(|(n, _)| n == name).count();
        suppliers == 1 && self.flats.iter().any(|(n, pins)| n == name && *pins)
    }
}

/// Where one candidate's imports point, relative to the importing file. A
/// generator's string arguments resolve to the modules or directory it mounts.
/// An import that does not resolve pins nothing. `overlays` holds every open
/// buffer, read in place of its file.
fn reaches(
    imports: &[ImportDecl],
    importer: Option<&str>,
    target_file: &str,
    opts: &vyrn_frontend::loader::LoadOptions,
    overlays: &HashMap<String, String>,
) -> Reaches {
    let mut r = Reaches::default();
    let resolve = |spec: &str| -> Option<String> {
        vyrn_frontend::loader::resolve_spec(spec, importer?, opts).ok()
    };
    for imp in imports {
        let targets: Vec<String> = match &imp.source {
            ImportSource::Path(spec) => resolve(spec).into_iter().collect(),
            ImportSource::Generator { args, .. } => args
                .iter()
                .filter_map(|a| match a {
                    Expr::Str(s, _) => resolve(s),
                    _ => None,
                })
                .collect(),
        };
        let exact = targets.iter().any(|t| same_file(t, target_file));
        let mut pins = exact;
        match &imp.source {
            ImportSource::Path(_) => {
                if exact {
                    if let Some(ns) = &imp.namespace {
                        r.direct_ns.push(ns.clone());
                    }
                }
            }
            ImportSource::Generator { .. } => {
                pins = exact
                    || targets.iter().any(|t| covers_dir(t, target_file))
                    || targets
                        .iter()
                        .any(|t| mounts_import(t, target_file, opts, overlays));
                if let Some(ns) = &imp.namespace {
                    if exact {
                        r.exact_gen_ns.push(ns.clone());
                    } else if pins {
                        // A directory reach, or a mounted module that imports
                        // the target (re-emitted wire types).
                        r.dir_gen_ns.push(ns.clone());
                    }
                }
            }
        }
        // A generator's selective binding lives only in generated modules, so
        // any proven reach pins it; a plain path needs the exact module.
        for n in &imp.names {
            r.flats.push((n.local().to_string(), pins));
        }
    }
    r
}

/// Whether a module the generator argument `resolved` mounts imports
/// `target_file` directly. One level only: a generated client re-emits the wire
/// types of the modules it mounts, not of their import closure. Reads the disk
/// (at most 64 entries), with open buffers in place of their files.
fn mounts_import(
    resolved: &str,
    target_file: &str,
    opts: &vyrn_frontend::loader::LoadOptions,
    overlays: &HashMap<String, String>,
) -> bool {
    let stem = resolved.strip_suffix(".vyrn").unwrap_or(resolved);
    let mut mounted: Vec<String> = Vec::new();
    if std::path::Path::new(resolved).is_file() {
        mounted.push(resolved.to_string());
    }
    if let Ok(entries) = std::fs::read_dir(stem) {
        for e in entries.flatten().take(64) {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "vyrn") {
                mounted.push(p.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    for file in mounted {
        let key = vyrn_frontend::origin::OriginMaps::norm_path_key(&file);
        let text = match overlays.get(&key) {
            Some(t) => t.clone(),
            None => match std::fs::read_to_string(&file) {
                Ok(t) => t,
                Err(_) => continue,
            },
        };
        let Ok(tokens) = vyrn_frontend::lexer::lex(&text) else {
            continue;
        };
        let (program, _) = vyrn_frontend::parser::parse_accum(tokens);
        for imp in &program.imports {
            if let ImportSource::Path(spec) = &imp.source {
                if let Ok(t) = vyrn_frontend::loader::resolve_spec(spec, &file, opts) {
                    if same_file(&t, target_file) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Whether `target_file` lies under the directory `resolved` names. A directory
/// argument resolves as the loader's file guess `<dir>.vyrn`, so the extension
/// is stripped first; the caller has already tried the exact file match.
fn covers_dir(resolved: &str, target_file: &str) -> bool {
    // As [`same_file`]: on Windows lowercase the whole path, because
    // `norm_path_key` lowercases every LSP-side key.
    let norm = |p: &str| {
        let s = p.replace('\\', "/");
        let s = if cfg!(windows) {
            s.to_lowercase()
        } else {
            let mut c = s.chars();
            match c.next() {
                Some(d) => d.to_ascii_lowercase().to_string() + c.as_str(),
                None => s,
            }
        };
        s.trim_matches('/').to_string()
    };
    let t = norm(target_file);
    let mut d = norm(resolved);
    if let Some(stem) = d.strip_suffix(".vyrn") {
        d = stem.to_string();
    }
    if d.is_empty() || t == d {
        return false;
    }
    t.starts_with(&d) && t.get(d.len()..).is_some_and(|rest| rest.starts_with('/'))
}

/// The project-wide edit that renames `target` to `new`.
///
/// `decl_text` is the declaring file's live text. `overlays` maps every open
/// buffer's slash path to its text, read in place of the file.
pub fn workspace_edit(
    target: &Target,
    decl_text: &str,
    new: &str,
    maps: &[MappedSymbol],
    decl_analysis: &Analysis,
    decl_uri: &Url,
    overlays: &HashMap<String, String>,
    opts: &vyrn_frontend::loader::LoadOptions,
) -> Result<WorkspaceEdit, String> {
    if !valid_identifier(new) {
        return Err(format!("`{new}` is not a valid identifier"));
    }
    if new == target.name {
        return Ok(WorkspaceEdit::default());
    }
    let names = wanted(target, new, maps)?;

    let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
    // The declaring file uses the cursor's own resolution, so a shadowing local
    // is excluded as it is from a highlight.
    let refs = references(decl_analysis, target.line, target.col);
    let edits: Vec<TextEdit> = refs
        .iter()
        .map(|r| edit_at(decl_text, r.line, r.col, r.end_col, new, 0))
        .collect();
    if !edits.is_empty() {
        changes.insert(decl_uri.clone(), edits);
    }

    for cand in candidates(&target.file, &names, overlays)? {
        if cand.uri == *decl_uri {
            continue;
        }
        let Ok(tokens) = vyrn_frontend::lexer::lex(&cand.body) else {
            continue;
        };
        let (program, _) = vyrn_frontend::parser::parse_accum(tokens);
        let importer = cand
            .uri
            .to_file_path()
            .ok()
            .map(|p| p.to_string_lossy().replace('\\', "/"));
        let reach = reaches(
            &program.imports,
            importer.as_deref(),
            &target.file,
            opts,
            overlays,
        );
        if reach.is_empty() {
            continue;
        }
        let analysis = analyze(&cand.body);
        let mut edits: Vec<TextEdit> = Vec::new();
        for w in &names {
            let (allowed, quals) = reach.qualifiers(w);
            if !allowed {
                continue;
            }
            // A bare occurrence counts only under [`Reaches::bare_is_ours`].
            let all = references_to(&analysis, &w.old, &quals);
            if reach.bare_is_ours(&w.old) {
                for r in all {
                    edits.push(edit_at(
                        &cand.file_text,
                        r.line,
                        r.col,
                        r.end_col,
                        &w.new,
                        cand.line_offset,
                    ));
                }
                continue;
            }
            let bare = references_to(&analysis, &w.old, &[]);
            for r in all {
                if bare.iter().any(|b| b.line == r.line && b.col == r.col) {
                    continue;
                }
                edits.push(edit_at(
                    &cand.file_text,
                    r.line,
                    r.col,
                    r.end_col,
                    &w.new,
                    cand.line_offset,
                ));
            }
        }
        edits.sort_by_key(|e| (e.range.start.line, e.range.start.character));
        edits.dedup_by_key(|e| (e.range.start.line, e.range.start.character));
        if !edits.is_empty() {
            changes.entry(cand.uri).or_default().extend(edits);
        }
    }
    Ok(WorkspaceEdit {
        changes: Some(changes),
        ..Default::default()
    })
}

/// One replacement, from a frontend 1-based char span plus the body's line
/// offset within `file_text`, sent as UTF-16 code units.
fn edit_at(
    file_text: &str,
    line: usize,
    col: usize,
    end_col: usize,
    new: &str,
    line_offset: usize,
) -> TextEdit {
    let l = (line + line_offset - 1) as u32;
    let line_text = crate::line_of_text(file_text, l as usize);
    TextEdit {
        range: Range {
            start: Position {
                line: l,
                character: crate::char_col_to_utf16(line_text, col - 1),
            },
            end: Position {
                line: l,
                character: crate::char_col_to_utf16(line_text, end_col - 1),
            },
        },
        new_text: new.to_string(),
    }
}

/// Every project source a rename might touch: the files under the declaration's
/// app root whose text contains one of `names`, since a reference is a token of
/// that spelling; `.vyx` files contribute their `<script>` bodies.
///
/// The cap is a refusal, not a truncation, and counts only those files: a root
/// of any size is walked, but a file that names the declaration and was never
/// read is a call site the rename never rewrites.
fn candidates(
    decl_file: &str,
    names: &[Wanted],
    overlays: &HashMap<String, String>,
) -> Result<Vec<Candidate>, String> {
    let decl = std::path::Path::new(decl_file);
    let Some(dir) = decl.parent() else {
        return Ok(Vec::new());
    };
    let root = crate::app_root_for(dir);
    let mut paths = Vec::new();
    crate::collect_sources(&root, 0, usize::MAX, &["vyrn", "vyx"], &mut paths);
    let mut files = Vec::new();
    for path in paths {
        let slash = path.to_string_lossy().replace('\\', "/");
        let Some(text) = overlays
            .get(&vyrn_frontend::origin::OriginMaps::norm_path_key(&slash))
            .cloned()
            .or_else(|| std::fs::read_to_string(&path).ok())
        else {
            continue;
        };
        if names.iter().any(|w| text.contains(&w.old)) {
            files.push((path, slash, text));
        }
    }
    if files.len() > MAX_RENAME_FILES {
        return Err(format!(
            "more than {MAX_RENAME_FILES} source files under {} mention `{}` — \
             rename cannot promise to reach every call site, so it has changed nothing",
            root.display(),
            names[0].old
        ));
    }
    let mut out = Vec::new();
    for (path, slash, text) in files {
        let Ok(uri) = Url::from_file_path(&path) else {
            continue;
        };
        if slash.ends_with(".vyx") {
            if let Some((body, line_offset)) = vyx_script(&text) {
                out.push(Candidate {
                    uri,
                    body,
                    line_offset,
                    file_text: text,
                });
            }
        } else {
            out.push(Candidate {
                uri,
                body: text.clone(),
                line_offset: 0,
                file_text: text,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vyrn_frontend::ast::Id;

    #[test]
    fn a_generated_name_keeps_its_prefix_and_swaps_the_declaration_it_stands_for() {
        // The three prefixed shapes std/rpc and std/http emit.
        assert_eq!(
            derive_generated("pastesCreate", "create", "add").as_deref(),
            Some("pastesAdd")
        );
        assert_eq!(
            derive_generated("rpcHandlePastesCreate", "create", "add").as_deref(),
            Some("rpcHandlePastesAdd")
        );
        assert_eq!(
            derive_generated("PathCreate", "create", "add").as_deref(),
            Some("PathAdd")
        );
        // A same-named stub (`http()`, `rpcInProcess`) and a re-emitted type.
        assert_eq!(
            derive_generated("create", "create", "add").as_deref(),
            Some("add")
        );
        assert_eq!(
            derive_generated("PasteList", "PasteList", "Pastes").as_deref(),
            Some("Pastes")
        );
    }

    #[test]
    fn a_name_the_derivation_does_not_model_refuses_rather_than_guessing() {
        assert_eq!(derive_generated("somethingElse", "create", "add"), None);
        // An empty prefix names no generator.
        assert_eq!(derive_generated("Create", "create", "add"), None);
    }

    #[test]
    fn only_a_bare_identifier_may_be_written() {
        assert!(valid_identifier("recent"));
        assert!(valid_identifier("_x9"));
        assert!(!valid_identifier(""));
        assert!(!valid_identifier("9lives"));
        assert!(!valid_identifier("two words"));
        assert!(!valid_identifier("has-dash"));
    }

    #[test]
    fn a_vyx_script_body_maps_back_by_line_addition() {
        let vyx =
            "<script>\nimport { recent } from \"./api\"\n</script>\n<template>\n</template>\n";
        let (body, off) = vyx_script(vyx).expect("a script section");
        assert_eq!(off, 0, "the body starts on the `<script>` line itself");
        // Body line 2 is the import; in the file it is also line 2.
        assert_eq!(
            body.lines().nth(1),
            Some("import { recent } from \"./api\"")
        );
    }

    /// References are found by token: a comment, a longer identifier and a
    /// string never move.
    #[test]
    fn an_importing_modules_references_are_tokens_and_not_text() {
        let src = "import { recent } from \"./api\"\n\
// recent is mentioned here\n\
fn recentRows() -> Int64 {\n    return 0\n}\n\
fn use() -> Int64 {\n    let s = \"recent\"\n    return recent()\n}\n";
        let a = analyze(src);
        let refs = references_to(&a, "recent", &[]);
        let lines: Vec<usize> = refs.iter().map(|r| r.line).collect();
        assert_eq!(
            lines,
            vec![1, 8],
            "the import binding and the call, nothing else: {refs:?}"
        );
    }

    /// A namespace-qualified reference is found only under the qualifier that
    /// names the declaring module.
    #[test]
    fn a_qualified_reference_needs_its_own_receiver() {
        let src = "import * as store from \"./store\"\n\
import * as other from \"./other\"\n\
fn f() -> Int64 {\n    other.listPastes()\n    return store.listPastes()\n}\n";
        let a = analyze(src);
        let refs = references_to(&a, "listPastes", &["store".to_string()]);
        assert_eq!(refs.len(), 1, "{refs:?}");
        assert_eq!(refs[0].line, 5);
    }

    fn ns_import(ns: &str, source: ImportSource) -> ImportDecl {
        ImportDecl {
            names: Vec::new(),
            namespace: Some(ns.to_string()),
            source,
            line: 1,
        }
    }

    fn flat_import(names: &[&str], source: ImportSource) -> ImportDecl {
        ImportDecl {
            names: names
                .iter()
                .map(|n| vyrn_frontend::ast::ImportName {
                    original: n.to_string(),
                    alias: None,
                })
                .collect(),
            namespace: None,
            source,
            line: 1,
        }
    }

    fn opts() -> vyrn_frontend::loader::LoadOptions {
        // Relative specifiers need no manifest or std root.
        vyrn_frontend::loader::LoadOptions::default()
    }

    fn gen(name: &str, arg: &str) -> ImportSource {
        ImportSource::Generator {
            name: name.to_string(),
            args: vec![Expr::Str(arg.to_string(), Id::NEW)],
            line: 1,
        }
    }

    const PASTES: &str = "/proj/src/server/pastes.vyrn";

    /// Renaming pastes' `create` while users.vyrn also declares `create`: the
    /// `u` namespace, aimed at another module, qualifies nothing.
    #[test]
    fn a_generator_namespace_qualifies_only_its_own_target_module() {
        let imports = vec![
            ns_import("p", gen("http", "./server/pastes")),
            ns_import("u", gen("http", "./server/users")),
        ];
        let r = reaches(
            &imports,
            Some("/proj/src/root.vyrn"),
            PASTES,
            &opts(),
            &HashMap::new(),
        );
        let direct = Wanted {
            old: "create".to_string(),
            new: "add".to_string(),
            direct: true,
            generated: false,
        };
        let derived = Wanted {
            old: "PathCreate".to_string(),
            new: "PathAdd".to_string(),
            direct: false,
            generated: true,
        };
        // `http("./server/pastes")` re-exports under the declaration's own
        // name AND carries derived spellings; `u` supplies neither.
        assert_eq!(r.qualifiers(&direct), (true, vec!["p".to_string()]));
        assert_eq!(r.qualifiers(&derived), (true, vec!["p".to_string()]));
    }

    #[test]
    fn a_directory_generator_reaches_derived_names_but_not_the_bare_one() {
        let imports = vec![ns_import("api", gen("client", "./server/api"))];
        // Not `PASTES`: `./server/api` covers only files inside it.
        let inside = "/proj/src/server/api/pastes.vyrn";
        let r = reaches(
            &imports,
            Some("/proj/src/root.vyrn"),
            inside,
            &opts(),
            &HashMap::new(),
        );
        let direct = Wanted {
            old: "create".to_string(),
            new: "add".to_string(),
            direct: true,
            generated: false,
        };
        let derived = Wanted {
            old: "pastesCreate".to_string(),
            new: "pastesAdd".to_string(),
            direct: false,
            generated: true,
        };
        // `client` exports `pastesCreate`, never the declaration's own name.
        assert_eq!(r.qualifiers(&direct), (false, Vec::<String>::new()));
        assert_eq!(r.qualifiers(&derived), (true, vec!["api".to_string()]));
    }

    #[test]
    fn a_bare_occurrence_counts_only_when_one_import_pins_the_module() {
        let pinned = flat_import(&["create"], ImportSource::Path("./server/pastes".into()));
        let other = flat_import(&["create"], ImportSource::Path("./server/users".into()));
        // Two suppliers of the spelling: which declaration a bare `create(`
        // denotes is unresolved, so it must be skipped.
        let both = reaches(
            &[pinned.clone(), other],
            Some("/proj/src/root.vyrn"),
            PASTES,
            &opts(),
            &HashMap::new(),
        );
        assert!(!both.bare_is_ours("create"));
        let one = reaches(
            &[pinned],
            Some("/proj/src/root.vyrn"),
            PASTES,
            &opts(),
            &HashMap::new(),
        );
        assert!(one.bare_is_ours("create"));
        // A namespace import binds no flat names.
        let ns_only = reaches(
            &[ns_import("p", ImportSource::Path("./server/pastes".into()))],
            Some("/proj/src/root.vyrn"),
            PASTES,
            &opts(),
            &HashMap::new(),
        );
        assert!(!ns_only.bare_is_ours("create"));
    }

    #[test]
    fn an_import_aimed_elsewhere_pins_nothing_and_qualifies_nothing() {
        let imports = vec![
            flat_import(&["create"], ImportSource::Path("./server/elsewhere".into())),
            ns_import("g", gen("http", "../outside/app")),
        ];
        let r = reaches(
            &imports,
            Some("/proj/src/root.vyrn"),
            PASTES,
            &opts(),
            &HashMap::new(),
        );
        let direct = Wanted {
            old: "create".to_string(),
            new: "add".to_string(),
            direct: true,
            generated: false,
        };
        assert_eq!(r.qualifiers(&direct), (false, Vec::<String>::new()));
        assert!(!r.bare_is_ours("create"));
    }

    #[test]
    fn a_directory_covers_only_its_own_subtree() {
        // A directory argument resolves as the loader's file guess `<dir>.vyrn`.
        assert!(covers_dir(
            "/p/server/api.vyrn",
            "/p/server/api/pastes.vyrn"
        ));
        assert!(covers_dir("/p/server/api", "/p/server/api/pastes.vyrn"));
        assert!(covers_dir("/p/server/api/", "/p/server/api/deep/x.vyrn"));
        assert!(!covers_dir("/p/server/api", "/p/server/api.vyrn"));
        assert!(!covers_dir("/p/server/api", "/p/server/api2/pastes.vyrn"));
        assert!(!covers_dir("/p/other", "/p/server/pastes.vyrn"));
    }

    /// `import { pastesCreate } from client("./server/api")`: a proven reach of
    /// the mount pins the selective binding.
    #[test]
    fn a_directory_generators_selective_binding_pins_its_bare_uses() {
        let imports = vec![flat_import(
            &["pastesCreate"],
            gen("client", "./server/api"),
        )];
        let inside = "/proj/src/server/api/pastes.vyrn";
        let r = reaches(
            &imports,
            Some("/proj/src/root.vyrn"),
            inside,
            &opts(),
            &HashMap::new(),
        );
        assert!(
            r.bare_is_ours("pastesCreate"),
            "one supplier, and it provably reaches the declaring module"
        );
        // Two suppliers of the spelling stay unresolved.
        let both = vec![
            flat_import(&["pastesCreate"], gen("client", "./server/api")),
            flat_import(&["pastesCreate"], ImportSource::Path("./elsewhere".into())),
        ];
        let r = reaches(
            &both,
            Some("/proj/src/root.vyrn"),
            inside,
            &opts(),
            &HashMap::new(),
        );
        assert!(!r.bare_is_ours("pastesCreate"));
    }

    /// The map's baked line lags the live line after an edit above `create`;
    /// entries match on name and file, so the drift still admits the entry.
    #[test]
    fn a_map_line_that_drifts_from_the_live_buffer_still_admits_the_spelling() {
        let target = Target {
            file: "/proj/src/server/pastes.vyrn".to_string(),
            name: "create".to_string(),
            line: 30,
            col: 4,
            end_col: 10,
        };
        let maps = vec![MappedSymbol {
            name: "pastesCreate".to_string(),
            file: "/proj/src/server/pastes.vyrn".to_string(),
            line: 28,
            col: 4,
            decl: "create".to_string(),
            derived: Vec::new(),
        }];
        let out = wanted(&target, "add", &maps).expect("the name derives");
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].old, "pastesCreate");
        assert_eq!(out[1].new, "pastesAdd");
        // A map aimed at ANOTHER file still contributes nothing.
        let elsewhere = vec![MappedSymbol {
            file: "/proj/src/server/users.vyrn".to_string(),
            ..maps[0].clone()
        }];
        let out = wanted(&target, "add", &elsewhere).expect("the direct spelling always is");
        assert_eq!(out.len(), 1, "only the declaration's own name");
    }
}
