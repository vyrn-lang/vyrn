//! The contract half of the editor, as an adapter over
//! [`vyrn_frontend::contracts`], which makes every decision. This module finds
//! the file's project context, caches it so a keystroke does not re-read
//! `std/ui.vyrn`, and turns frontend shapes into LSP shapes. It names no
//! contract, so a third-party contract gets the same editor support.

use std::collections::HashMap;

use vyrn_frontend::contracts::{ContractView, Role};

/// A project's resolved contract knowledge, cached per app directory.
pub struct ContractIndex {
    /// Signature of `vyrn.json` and the fallback roots; a change re-derives the roles.
    pub sig: u64,
    /// Whether [`Self::roles`] is derived at [`Self::sig`]. An empty role list is
    /// cached too, so a project without roles does not re-parse on each keystroke.
    pub derived: bool,
    pub roles: Vec<Role>,
    /// `module:Contract` -> (declaring file's signature, the view). Each lookup
    /// re-checks the signature, so an edited contract module needs no restart.
    pub views: HashMap<String, (u64, ContractView)>,
}

/// The (len, mtime-nanos) signature of one file, folded into a `u64`; 0 for a
/// missing file.
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

/// The `<script> ... </script>` body of a `.vyx`, and how many lines to subtract
/// from a buffer line to reach the same line of that body.
///
/// The body starts mid-line after the tag's `>`, so the offset counts newlines
/// before it, not lines.
pub fn vyx_script(text: &str) -> Option<(String, usize)> {
    let (body_start, close) = vyrn_frontend::vyx::script_body(text)?;
    let line_offset = text[..body_start].matches('\n').count();
    Some((text[body_start..close].to_string(), line_offset))
}

/// The identifier token covering the 1-based `(line, col)` cursor in `text`,
/// with its 1-based column span. A text scan, because a `.vyx` buffer is not a
/// Vyrn document.
pub fn ident_at(text: &str, line: usize, col: usize) -> Option<(String, usize, usize)> {
    let src = text.lines().nth(line.checked_sub(1)?)?;
    let chars: Vec<char> = src.chars().collect();
    // The lexer's identifier class (`lexer.rs`), not ASCII: a query on a name
    // with an accented letter must find it.
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
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

/// The names a module already exports, which completion must not offer again.
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
