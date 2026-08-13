//! RFC-0071 M4 — the contract half of the editor, as an adapter.
//!
//! Every decision here is a call into [`vyrn_frontend::contracts`]: which
//! contract governs a file, what its members are, what to complete, what to
//! hover, where to jump, what to rename. This module's whole job is finding the
//! file's project context, caching the answer so a keystroke does not re-read
//! `std/ui.vyrn`, and turning frontend shapes into LSP shapes.
//!
//! Nothing about `Page`, `Component`, `head` or `data` appears below. A
//! third-party generator that declares its own contract gets the same
//! completion, hover, go-to-definition and quick-fix with no change here — which
//! is the claim RFC-0071 makes and this is where it is either true or not.

use std::collections::HashMap;

use vyrn_frontend::contracts::{ContractView, Role};
use vyrn_frontend::loader::LoadOptions;

/// A project's resolved contract knowledge, cached per app directory.
pub struct ContractIndex {
    /// `vyrn.json`'s signature (len + mtime) when this was built, plus the roots
    /// scanned for the fallback. A change re-derives the roles.
    pub sig: u64,
    /// Whether [`Self::roles`] has been derived at [`Self::sig`]. A project with
    /// genuinely no roles (a scratch file, a library) must cache that answer too
    /// — otherwise every keystroke in it re-parses the app's roots to rediscover
    /// nothing.
    pub derived: bool,
    pub roles: Vec<Role>,
    /// `module:Contract` → (declaring file's signature, the view). The file
    /// signature is re-checked on every lookup, so editing `std/ui.vyrn` (or a
    /// user's own contract module) is picked up without restarting the server.
    pub views: HashMap<String, (u64, ContractView)>,
}

/// The (len, mtime-nanos) signature of one file, folded into a `u64`. Missing
/// files fold to 0 — a contract module that vanished simply re-resolves.
pub fn file_sig(path: &std::path::Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let Ok(md) = std::fs::metadata(path) else {
        return 0;
    };
    let mut h = std::collections::hash_map::DefaultHasher::new();
    md.len().hash(&mut h);
    if let Ok(t) = md.modified() {
        if let Ok(d) = t.duration_since(std::time::UNIX_EPOCH) {
            d.as_nanos().hash(&mut h);
        }
    }
    h.finish()
}

/// The `.vyrn` modules a project's roles are discovered from: the manifest's
/// declared entry points (`main`, `server`, `client`) plus every `.vyrn` file
/// sitting directly in the app directory.
///
/// Generator imports live in an app's ROOT modules by construction — a page
/// tree is consumed by the server and the client roots, never by a page. So
/// this is a shallow, bounded scan (no recursive walk), which is what keeps the
/// fallback affordable.
pub fn role_roots(app_dir: &std::path::Path) -> Vec<(String, String)> {
    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    if let Some(doc) = crate::manifest_doc(app_dir) {
        for key in ["main", "server", "client"] {
            if let Some(vyrn_frontend::schema::Json::Str(p)) = doc.get(key) {
                paths.push(app_dir.join(p));
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir(app_dir) {
        for e in entries.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("vyrn") {
                paths.push(p);
            }
        }
    }
    paths.sort();
    paths.dedup();
    let mut out = Vec::new();
    for p in paths {
        if let Ok(src) = std::fs::read_to_string(&p) {
            out.push((p.to_string_lossy().replace('\\', "/"), src));
        }
    }
    out
}

/// The signature the role table was derived at: `vyrn.json` plus every root.
pub fn roles_sig(app_dir: &std::path::Path, roots: &[(String, String)]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    file_sig(&app_dir.join("vyrn.json")).hash(&mut h);
    for (p, _) in roots {
        p.hash(&mut h);
        file_sig(std::path::Path::new(p)).hash(&mut h);
    }
    h.finish()
}

/// Derive a project's roles: the manifest's `roles` key when it has one
/// (RFC-0071's form, which RFC-0072 inherits), else discovery from the
/// generator call sites the app already writes.
pub fn roles_of(
    app_dir: &std::path::Path,
    roots: &[(String, String)],
    opts: &LoadOptions,
    resolver: &dyn vyrn_frontend::loader::ModuleResolver,
) -> Vec<Role> {
    if let Some(doc) = crate::manifest_doc(app_dir) {
        let declared = vyrn_frontend::contracts::roles_from_manifest(&doc);
        if !declared.is_empty() {
            return declared;
        }
    }
    vyrn_frontend::contracts::discovered_roles(roots, opts, resolver)
}

/// The `<script> … </script>` body of a `.vyx`, and how many lines to subtract
/// from a buffer line to reach the same line of that body.
///
/// A `.vyx` script is ordinary Vyrn, so every contract query works on it
/// unchanged once the lines line up. The body starts immediately after the
/// `<script>` tag's `>`, mid-line, so the offset is the number of NEWLINES
/// before it — not the number of lines, which would be one too many and put
/// every query one line off.
pub fn vyx_script(text: &str) -> Option<(String, usize)> {
    let open_tag = text.find("<script")?;
    let body_start = text[open_tag..].find('>')? + open_tag + 1;
    let close = text[body_start..].find("</script>")? + body_start;
    let line_offset = text[..body_start].matches('\n').count();
    Some((text[body_start..close].to_string(), line_offset))
}

/// The identifier token covering the 1-based `(line, col)` cursor in `text`,
/// with its 1-based column span. A plain text scan: the caller may be looking at
/// a `.vyx` buffer, which is not a Vyrn document.
pub fn ident_at(text: &str, line: usize, col: usize) -> Option<(String, usize, usize)> {
    let src = text.lines().nth(line.checked_sub(1)?)?;
    let chars: Vec<char> = src.chars().collect();
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let cur = col.saturating_sub(1).min(chars.len());
    let mut start = cur;
    while start > 0 && chars.get(start - 1).is_some_and(|&c| is_ident(c)) {
        start -= 1;
    }
    let mut end = cur;
    while end < chars.len() && chars.get(end).is_some_and(|&c| is_ident(c)) {
        end += 1;
    }
    if start >= end {
        return None;
    }
    Some((chars[start..end].iter().collect(), start + 1, end + 1))
}

/// The names a module already exports — what completion must not offer again.
pub fn exported_names(source: &str) -> Vec<String> {
    let Ok(tokens) = vyrn_frontend::lexer::lex(source) else {
        return Vec::new();
    };
    let (program, _) = vyrn_frontend::parser::parse_accum(tokens);
    program
        .functions
        .iter()
        .filter(|f| f.exported)
        .map(|f| f.name.clone())
        .collect()
}
