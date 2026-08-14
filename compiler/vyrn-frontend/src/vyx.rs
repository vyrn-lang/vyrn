//! Where a `.vyx` component's `<script>` section ends — the Rust side of a rule
//! `std/vyx` owns.
//!
//! **The rule.** A `.vyx` is not markup with code pasted in: the `<script>` body
//! is Vyrn, so its closing tag is found by scanning the body AS VYRN. A
//! `</script>` inside a string literal or a comment closes nothing (RFC-0039
//! §2). The naive `find("</script>")` truncates such a file mid-literal, and
//! every answer computed from the truncated text is a lie about a component that
//! compiles and runs.
//!
//! **The authority is `std/vyx`.** `vyxSection` / `vyxScanFindCode`
//! (`std/vyx.vyrn`) decide the boundary for the compiler; this module is the
//! same walk in Rust, for the tools that read a `.vyx` without running the
//! generator (`vyrn why`, contract discovery, LSP rename / completion). The two
//! cannot share an implementation — one runs as compiled Vyrn inside the
//! generator, the other inside the toolchain — so they agree by transliteration,
//! and `audit_hostile_sections_agree_with_the_generator` (`vyrn-cli/tests/vyx.rs`)
//! fails if either drifts.
//!
//! Deliberately identical to the authority, oddities included:
//!
//!   * a byte literal (`'"'`) is recognized by neither scanner, so a script that
//!     holds one and a later `</script>` is mis-split by both;
//!   * `vyxScanFindCode` skips `/* … */`, and Vyrn has no block comments, so
//!     that arm is unreachable in source the compiler would accept.
//!
//! Neither is fixed here. Fixing one alone would make the tools disagree with
//! the compiler, which is the defect this module exists to remove — change
//! `vyxScanFindCode` first, then follow it here.

/// The byte range of the `<script>` body in a `.vyx` source: from just after the
/// open tag's `>` to the `<` of the `</script>` that closes it, per the scanner
/// rule above. `None` when the file has no closed `<script>` section — including
/// a `<script>` whose only `</script>` sits inside a string or comment, which
/// `std/vyx` rejects too.
///
/// The open tag is the literal `<script>`, exactly as `std/vyx` looks for it: a
/// `.vyx` whose tag carries attributes has no section for the compiler either.
pub fn script_body(text: &str) -> Option<(usize, usize)> {
    const OPEN: &str = "<script>";
    const CLOSE: &[u8] = b"</script>";
    let start = text.find(OPEN)? + OPEN.len();
    let end = find_in_code(text.as_bytes(), CLOSE, start)?;
    Some((start, end))
}

/// The first `needle` at `from` or later that is CODE — not inside a `"…"`
/// string, a `//` line comment or a `/* … */` block comment. A transliteration
/// of `vyxScanFindCode` (`std/vyx.vyrn`); keep the two the same walk.
///
/// The needle is ASCII, so every hit starts at a UTF-8 character boundary and
/// the caller may slice on it.
fn find_in_code(ba: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i < ba.len() {
        match ba[i] {
            // A `"…"` string literal: skip to the unescaped closing quote.
            b'"' => {
                i += 1;
                while i < ba.len() && ba[i] != b'"' {
                    if ba[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i += 1;
            }
            // A `//` line comment: skip to the next LF.
            b'/' if ba.get(i + 1) == Some(&b'/') => {
                i += 2;
                while i < ba.len() && ba[i] != b'\n' {
                    i += 1;
                }
            }
            // A `/* … */` block comment: skip to the closing `*/`.
            b'/' if ba.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < ba.len() && !(ba[i] == b'*' && ba[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
            }
            _ => {
                if ba[i..].starts_with(needle) {
                    return Some(i);
                }
                i += 1;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(text: &str) -> Option<&str> {
        script_body(text).map(|(s, e)| &text[s..e])
    }

    #[test]
    fn a_plain_section_is_its_body() {
        assert_eq!(
            body("<script>\nlet a = 1\n</script>\n"),
            Some("\nlet a = 1\n")
        );
    }

    #[test]
    fn a_close_tag_inside_a_string_does_not_close_the_section() {
        let t = "<script>\nfn tag() -> String { return \"</script>\" }\nprops { n: Int64 }\n</script>\n<template><li>x</li></template>\n";
        let b = body(t).expect("a closed section");
        assert!(b.contains("props { n: Int64 }"), "truncated: {b:?}");
        assert!(!b.contains("<template"), "ran past the section: {b:?}");
    }

    #[test]
    fn a_close_tag_inside_a_comment_does_not_close_the_section() {
        let line = "<script>\n// </script>\nlet a = 1\n</script>\n";
        assert!(body(line).expect("closed").contains("let a = 1"), "{line}");
        // Vyrn has no block comments, so this is not source the compiler would
        // accept — the arm exists only because `vyxScanFindCode` has it, and
        // this pins that the two still walk it the same way.
        let block = "<script>\n/* </script> */\nlet a = 1\n</script>\n";
        assert!(
            body(block).expect("closed").contains("let a = 1"),
            "{block}"
        );
    }

    #[test]
    fn an_escaped_quote_does_not_end_the_string() {
        // The `\"` keeps the string open, so the `</script>` after it is still
        // inside it; the real close is the last one.
        let t = "<script>\nlet s = \"a\\\"</script>b\"\nlet n = 1\n</script>\n";
        let b = body(t).expect("a closed section");
        assert!(b.contains("let n = 1"), "truncated: {b:?}");
    }

    #[test]
    fn no_section_is_none() {
        assert_eq!(script_body("<template><p>x</p></template>\n"), None);
        // Open, never closed in code: the only `</script>` is inside a string.
        assert_eq!(script_body("<script>\nlet s = \"</script>\"\n"), None);
    }
}
