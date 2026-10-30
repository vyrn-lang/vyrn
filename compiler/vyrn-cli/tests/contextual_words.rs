//! The playground (`compiler/vyrn-play/src/lib.rs`) and the site highlighter
//! (`site/app/hl.vyrn`) each hold a list of contextual words. The grammar is the
//! home of those words, as [`CONTEXTUAL`] writes them, so each list is held to it.

use std::path::{Path, PathBuf};
use vyrn_frontend::lexer::CONTEXTUAL;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// Every double-quoted word between `open` and the next `]`, in source order.
fn words_after(src: &str, open: &str) -> Vec<String> {
    let at = src
        .find(open)
        .unwrap_or_else(|| panic!("`{open}` is gone — this test needs a new anchor"));
    // Search past the anchor: the anchor's `&[&str]` holds a `]` of its own.
    let body = &src[at + open.len()..];
    let end = body
        .find(']')
        .unwrap_or_else(|| panic!("`{open}` has no closing bracket"));
    let mut out = Vec::new();
    let mut rest = &body[..end];
    while let Some(a) = rest.find('"') {
        let after = &rest[a + 1..];
        let Some(b) = after.find('"') else { break };
        out.push(after[..b].to_string());
        rest = &after[b + 1..];
    }
    out
}

/// The words the highlighter at `file` lists after `open` that the grammar does
/// not state.
fn strays(file: &str, open: &str) -> Vec<String> {
    let src = std::fs::read_to_string(repo_root().join(file)).expect("the highlighter");
    let mut words = words_after(&src, open);
    words.retain(|w| !CONTEXTUAL.contains(&w.as_str()));
    words
}

/// Both highlighters colour the builtin `panic` as a word. A builtin is a name the
/// checker knows, so no rule of the grammar states it.
const BUILTIN: &[&str] = &["panic"];

#[test]
fn the_playground_colours_only_the_grammars_contextual_words() {
    let open = "const CONTEXTUAL: &[&str] = &[";
    assert_eq!(strays("compiler/vyrn-play/src/lib.rs", open), BUILTIN);
}

#[test]
fn the_site_colours_only_the_grammars_contextual_words() {
    let open = "fn contextual() -> Array<String> {\n    return [";
    assert_eq!(strays("site/app/hl.vyrn", open), BUILTIN);
}
