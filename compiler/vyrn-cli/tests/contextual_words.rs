//! The playground (`compiler/vyrn-play/src/lib.rs`) and the site highlighter
//! (`site/app/hl.vyrn`) colour the same contextual words. Neither can import
//! the other (one is `wasm32-unknown-unknown` Rust, the other a Vyrn
//! generator), so the list is written twice and this test stops the copies
//! drifting. The editor grammar answers a different question (words in a
//! `.vyrn` file) and is checked against the lexer by
//! `editor/vscode/test/grammar.test.mjs`.

use std::path::{Path, PathBuf};

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

#[test]
fn the_playground_and_the_site_colour_the_same_contextual_words() {
    let root = repo_root();
    let play = std::fs::read_to_string(root.join("compiler/vyrn-play/src/lib.rs"))
        .expect("the playground crate");
    let site =
        std::fs::read_to_string(root.join("site/app/hl.vyrn")).expect("the site highlighter");

    let a = words_after(&play, "const CONTEXTUAL: &[&str] = &[");
    let b = words_after(&site, "fn contextual() -> Array<String> {\n    return [");

    assert!(
        a.len() >= 8,
        "only {} words read from the playground — the shape changed",
        a.len()
    );
    assert_eq!(
        a, b,
        "the playground and the site disagree about which words are contextual:\n  \
         playground: {a:?}\n  site:       {b:?}"
    );
}
