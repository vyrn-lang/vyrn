//! The two highlighters colour the same contextual words.
//!
//! `read`, `modify`, `consume` and `share` are the language's whole ownership
//! surface, and a page that invites you to type them cannot render them as
//! ordinary names. Two highlighters agree about that today and nothing checks
//! that they keep agreeing:
//!
//! - `compiler/vyrn-play/src/lib.rs` — the playground, Rust, an excluded crate.
//! - `site/app/hl.vyrn` — the site's own highlighter, Vyrn.
//!
//! Neither can import the other. The playground compiles to
//! `wasm32-unknown-unknown` and the site's is a Vyrn generator, so the list is
//! written twice on purpose. This test is what stops the two copies drifting:
//! a word added to one and not the other fails here, and the failure names it.
//!
//! **The editor grammar is deliberately not compared.** Its contextual list
//! answers a different question — which words to colour in a `.vyrn` FILE — and
//! it holds `extern`, `lazy`, `place`, `yield` and `logging`, which these two do
//! not, while leaving out `consume`, `share` and `panic`, which these two carry.
//! `editor/vscode/test/grammar.test.mjs` checks that one against the lexer.

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
    // PAST the anchor, not from it: `&[&str]` carries a `]` of its own, and
    // searching from the anchor's start ended the list inside its own type.
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
