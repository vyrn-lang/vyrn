//! Cross-boundary rename (RFC-0073 M4).
//!
//! Renaming a procedure in `server/api/pastes.vyrn` has to reach places the
//! declaration cannot see. `client("./server/api")` exports it as `pastesCreate`
//! and a page calls `api.pastesCreate(..)`; `rpc(..)` dispatches to
//! `rpcHandlePastesCreate`; `http("./pastes")` re-exports it under its own name
//! for the REST projection. None of those spellings appears in the file being
//! edited, and none of the files that use them imports it directly.
//!
//! M1's symbol map is what closes the gap, read in the direction M3 did not use:
//! M3 asked "which declaration is this generated symbol", this asks "which
//! generated symbols is this declaration". Every mapped symbol whose origin is
//! the declaration under the cursor gets a NEW generated name derived the way the
//! generator derives it, and the references to the old one are rewritten.
//!
//! **Generated modules are never edited.** They are build artifacts: the edit
//! lands in sources only, and the next load regenerates them from the renamed
//! declaration. That is also why the derivation has to be predicted rather than
//! observed — the module carrying the new name does not exist yet.
//!
//! ### What this reaches, and what it does not
//!
//! Reference collection is [`vyrn_frontend::references_to`] over each candidate
//! file's own tokens, so it is scope-aware within a file and never a textual
//! search. The candidate set is the project's `.vyrn` files and the `<script>`
//! bodies of its `.vyx` files, filtered to those that IMPORT either the
//! declaring module or a generator — the only modules in which either name can
//! occur, since Vyrn re-exports nothing implicitly.
//!
//! Two classes are out of reach, both reported rather than papered over:
//!
//! * A `.vyx` TEMPLATE expression (`{{ .. }}`, `v-if="..."`). Only the script
//!   body is lexed; a template calls the page's own view helpers rather than an
//!   api procedure, so this has no occurrence in the corpus — but it is a real
//!   hole and a missed one is a build error, not a silent miss.
//! * The WIRE. Renaming a procedure moves its derived path, so an external HTTP
//!   client keeps calling the old one. Nothing in the tree can see that, and the
//!   rename does not pretend to.
//!
//! A name whose generated spelling cannot be derived REFUSES the whole rename
//! rather than performing part of it — see [`derive_generated`].

use std::collections::HashMap;

use lsp_types::{Position, PrepareRenameResponse, Range, TextEdit, Url, WorkspaceEdit};
use vyrn_frontend::ast::ImportSource;
use vyrn_frontend::symbolmap::{same_file, MappedSymbol};
use vyrn_frontend::{analyze, references, references_to, Analysis, SymbolKind};

/// The most project files a rename will read. Higher than the `.vyx` owner
/// probe's cap because a rename is a once-per-refactor action rather than a
/// per-keystroke one, and a miss here is a broken build.
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
/// Deliberately narrow: a TOP-LEVEL declaration in the open document, at its own
/// name. A use of the name inside a body resolves to the same declaration, but
/// renaming from a use would have to re-derive which declaration it is in every
/// candidate file, and the cursor is one keypress from the declaration anyway.
pub fn target_at(
    analysis: &Analysis,
    file: &str,
    line: usize,
    col: usize,
) -> Result<Target, String> {
    // A FOREIGN symbol resolves here too — an imported name, or a generated stub
    // whose "file" is the generating module's banner. Renaming one in place is
    // exactly the mistake this milestone exists to make unnecessary, so it is
    // named rather than silently declined.
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
/// current name.
///
/// Worth answering rather than leaving to the client's default. Without it VS
/// Code takes the word under the cursor — any word, in any file, including one
/// inside a comment or a `.vyx` template — and only discovers the server will not
/// rename it after the user has typed a replacement. With it the refusal, and its
/// reason, arrive before the box opens.
pub fn prepare(target: &Target) -> PrepareRenameResponse {
    PrepareRenameResponse::RangeWithPlaceholder {
        range: Range {
            start: Position {
                line: (target.line - 1) as u32,
                character: (target.col - 1) as u32,
            },
            end: Position {
                line: (target.line - 1) as u32,
                character: (target.end_col - 1) as u32,
            },
        },
        placeholder: target.name.clone(),
    }
}

/// Whether `name` is a bare identifier — the only thing a rename may write.
/// Checked because every edit below is a blind textual replacement of a token
/// span: `fn recent()` renamed to `recent paste` would not fail to compile, it
/// would fail to LEX, in as many files as the rename touched.
pub fn valid_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `create` → `Create`, the generators' own `capFirst`.
fn cap_first(s: &str) -> String {
    let mut cs = s.chars();
    match cs.next() {
        Some(c) => c.to_uppercase().collect::<String>() + cs.as_str(),
        None => String::new(),
    }
}

/// The name a generated symbol takes once its declaration is renamed.
///
/// Every generator that emits a map builds its names the same way — a prefix it
/// chose, then `capFirst` of the declaration's name: `client()` exports
/// `pastesCreate`, `rpc()` dispatches to `rpcHandlePastesCreate`, `http()`
/// checks paths with `PathCreate`, and a re-emitted type or a same-named stub
/// carries the declaration's name unchanged. So the new name is the old one with
/// that suffix swapped, and the prefix — which encodes the api-relative
/// directory, not the procedure — is preserved untouched.
///
/// `None` when the generated name does not end in the declaration's name at all.
/// That is a generator this predicate does not model, and the honest answer is to
/// refuse the rename: a rename that silently skipped it would leave a call site
/// pointing at a symbol nothing generates any more, and the build error would be
/// blamed on the wrong file.
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
/// Both flags can be set for the same spelling, and routinely are: `http()`
/// re-exports a procedure under the DECLARATION's own name, so in a projection
/// file `create` is reachable as an import of the declaring module and as an
/// import of the generated one. Two entries would rewrite the same span twice.
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
/// The map is filtered to the declaration's own `(name, line)` — the agreement
/// M1 put under test, where the client's stub and the server's handler name the
/// same declaration at the same place, is what makes both reachable from one
/// cursor.
fn wanted(target: &Target, new: &str, maps: &[MappedSymbol]) -> Result<Vec<Wanted>, String> {
    let mut out = vec![Wanted {
        old: target.name.clone(),
        new: new.to_string(),
        direct: true,
        generated: false,
    }];
    for m in maps {
        if m.decl != target.name || m.line != target.line || !same_file(&m.file, &target.file) {
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

/// A candidate file, ready to search: its Vyrn body, the line the body starts at
/// within the file, and the imports that body declares.
struct Candidate {
    uri: Url,
    body: String,
    line_offset: usize,
}

/// The `<script> … </script>` body of a `.vyx` and the number of file lines
/// before it, so a position in the body maps back by addition. `None` for a
/// `.vyx` with no script section.
///
/// Columns need no adjustment: the body starts immediately after the `<script>`
/// tag's `>`, so its first line is that tag's (empty) remainder and every line
/// after it is a whole line of the file.
fn vyx_script(text: &str) -> Option<(String, usize)> {
    let open = text.find("<script")?;
    let start = text[open..].find('>')? + open + 1;
    let close = text[start..].find("</script>")? + start;
    let before = text[..start].matches('\n').count();
    Some((text[start..close].to_string(), before))
}

/// Rewrite `wanted` throughout the project and return the edit.
///
/// `overlays` is every open buffer (slash path → text), so an unsaved edit is
/// renamed as it stands rather than as it was last saved.
pub fn workspace_edit(
    target: &Target,
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
    // The declaring file answers for itself, with the cursor the request carried:
    // this is RFC-0050's own resolution, so a local that shadows the name inside
    // some body is excluded exactly where the editor already excludes it from a
    // highlight.
    let refs = references(decl_analysis, target.line, target.col);
    let edits: Vec<TextEdit> = refs
        .iter()
        .map(|r| edit_at(r.line, r.col, r.end_col, new, 0))
        .collect();
    if !edits.is_empty() {
        changes.insert(decl_uri.clone(), edits);
    }

    for cand in candidates(&target.file, overlays)? {
        if cand.uri == *decl_uri {
            continue;
        }
        let Ok(tokens) = vyrn_frontend::lexer::lex(&cand.body) else {
            continue;
        };
        let (program, _) = vyrn_frontend::parser::parse_accum(tokens);
        // Which of the wanted names can occur here at all, and under which
        // qualifiers. A module that imports neither the declaration nor a
        // generator cannot mention either spelling, and is not analyzed.
        let mut direct_ns: Vec<String> = Vec::new();
        let mut gen_ns: Vec<String> = Vec::new();
        let mut imports_decl = false;
        let mut imports_gen = false;
        let importer = cand
            .uri
            .to_file_path()
            .ok()
            .map(|p| p.to_string_lossy().replace('\\', "/"));
        for imp in &program.imports {
            match &imp.source {
                ImportSource::Path(spec) => {
                    let Some(importer) = importer.as_deref() else {
                        continue;
                    };
                    let Ok(resolved) = vyrn_frontend::loader::resolve_spec(spec, importer, opts)
                    else {
                        continue;
                    };
                    if !same_file(&resolved, &target.file) {
                        continue;
                    }
                    imports_decl = true;
                    if let Some(ns) = &imp.namespace {
                        direct_ns.push(ns.clone());
                    }
                }
                ImportSource::Generator { .. } => {
                    imports_gen = true;
                    if let Some(ns) = &imp.namespace {
                        gen_ns.push(ns.clone());
                    }
                }
            }
        }
        if !imports_decl && !imports_gen {
            continue;
        }
        let analysis = analyze(&cand.body);
        let mut edits: Vec<TextEdit> = Vec::new();
        for w in &names {
            let mut quals: Vec<String> = Vec::new();
            let mut allowed = false;
            if w.direct && imports_decl {
                allowed = true;
                quals.extend(direct_ns.iter().cloned());
            }
            if w.generated && imports_gen {
                allowed = true;
                quals.extend(gen_ns.iter().cloned());
            }
            if !allowed {
                continue;
            }
            for r in references_to(&analysis, &w.old, &quals) {
                edits.push(edit_at(r.line, r.col, r.end_col, &w.new, cand.line_offset));
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

/// One replacement, from a frontend 1-based span plus the body's line offset
/// within its file.
fn edit_at(line: usize, col: usize, end_col: usize, new: &str, line_offset: usize) -> TextEdit {
    let l = (line + line_offset - 1) as u32;
    TextEdit {
        range: Range {
            start: Position {
                line: l,
                character: (col - 1) as u32,
            },
            end: Position {
                line: l,
                character: (end_col - 1) as u32,
            },
        },
        new_text: new.to_string(),
    }
}

/// Every project source a rename might touch: the `.vyrn` files under the
/// declaration's app root, and the `<script>` bodies of its `.vyx` files.
///
/// The cap is a refusal and not a truncation. A file the walk never reached is a
/// call site the rename never rewrites, and the only symptom would be a build
/// error in a file nobody was editing — so a project that outgrows the walk is
/// told, rather than half-renamed.
fn candidates(
    decl_file: &str,
    overlays: &HashMap<String, String>,
) -> Result<Vec<Candidate>, String> {
    let decl = std::path::Path::new(decl_file);
    let Some(dir) = decl.parent() else {
        return Ok(Vec::new());
    };
    let root = crate::app_root_for(dir);
    let mut files = Vec::new();
    crate::collect_sources(&root, 0, MAX_RENAME_FILES, &["vyrn", "vyx"], &mut files);
    if files.len() >= MAX_RENAME_FILES {
        return Err(format!(
            "this project has more than {MAX_RENAME_FILES} source files under {} — \
             rename cannot promise to reach every call site, so it has changed nothing",
            root.display()
        ));
    }
    let mut out = Vec::new();
    for path in files {
        let slash = path.to_string_lossy().replace('\\', "/");
        let text = match overlays
            .get(&vyrn_frontend::origin::OriginMaps::norm_path_key(&slash))
            .cloned()
            .or_else(|| std::fs::read_to_string(&path).ok())
        {
            Some(t) => t,
            None => continue,
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            continue;
        };
        if slash.ends_with(".vyx") {
            if let Some((body, line_offset)) = vyx_script(&text) {
                out.push(Candidate {
                    uri,
                    body,
                    line_offset,
                });
            }
        } else {
            out.push(Candidate {
                uri,
                body: text,
                line_offset: 0,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_name_keeps_its_prefix_and_swaps_the_declaration_it_stands_for() {
        // The three shapes std/rpc and std/http emit.
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
        // The prefix would be empty: `Create` alone says nothing about which
        // generator built it, so there is no defensible new name.
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

    /// The property the whole milestone rests on: a declaration's references in
    /// an importing module are found by TOKEN, so a comment, a longer identifier
    /// and a string never move.
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
}
