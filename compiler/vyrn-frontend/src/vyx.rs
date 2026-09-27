//! Where a `.vyx` component's `<script>` section ends: the Rust side of a rule
//! `std/vyx` owns.
//!
//! The `<script>` body is Vyrn, so a `</script>` inside a string or comment
//! closes nothing. `vyxSection` and `vyxScanFindCode`
//! (`std/vyx.vyrn`) decide the boundary for the compiler; this is the same walk
//! for tools that read a `.vyx` without running the generator (`vyrn why`,
//! contract discovery, the LSP). The two agree by transliteration, and
//! `audit_hostile_sections_agree_with_the_generator` (`vyrn-cli/tests/vyx.rs`)
//! fails if either drifts.
//!
//! The oddities are copied too: neither scanner knows a byte literal (`'"'`),
//! so a script holding one before a later `</script>` is mis-split by both, and
//! both skip `/* ... */`, which Vyrn does not have. Fix `vyxScanFindCode`
//! first, then follow it here.

/// Returns the byte range of the `<script>` body: from after the open tag's
/// `>` to the `<` of the `</script>` that closes it in code. `None` without a
/// closed section, including when the only `</script>` is in a string or
/// comment. The open tag is the literal `<script>`, as `std/vyx` looks for it,
/// so a tag with attributes opens no section.
pub fn script_body(text: &str) -> Option<(usize, usize)> {
    const OPEN: &str = "<script>";
    const CLOSE: &[u8] = b"</script>";
    let start = text.find(OPEN)? + OPEN.len();
    let end = find_in_code(text.as_bytes(), CLOSE, start)?;
    Some((start, end))
}

/// Returns the first `needle` at `from` or later that is code, not inside a
/// `"..."` string, a `//` comment or a `/* ... */` comment: the walk of
/// `vyxScanFindCode`. The needle is ASCII, so a hit is a UTF-8 boundary.
fn find_in_code(ba: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i < ba.len() {
        match ba[i] {
            // A string literal: skip to the unescaped closing quote.
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
            // A `//` comment: skip to the next LF.
            b'/' if ba.get(i + 1) == Some(&b'/') => {
                i += 2;
                while i < ba.len() && ba[i] != b'\n' {
                    i += 1;
                }
            }
            // A `/* ... */` comment: skip past the closing `*/`.
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
        // Vyrn has no block comments; this pins that both scanners walk the arm the
        // same way.
        let block = "<script>\n/* </script> */\nlet a = 1\n</script>\n";
        assert!(
            body(block).expect("closed").contains("let a = 1"),
            "{block}"
        );
    }

    #[test]
    fn an_escaped_quote_does_not_end_the_string() {
        // The `\"` keeps the string open, so the real close is the last one.
        let t = "<script>\nlet s = \"a\\\"</script>b\"\nlet n = 1\n</script>\n";
        let b = body(t).expect("a closed section");
        assert!(b.contains("let n = 1"), "truncated: {b:?}");
    }

    #[test]
    fn no_section_is_none() {
        assert_eq!(script_body("<template><p>x</p></template>\n"), None);
        // The only `</script>` is inside a string.
        assert_eq!(script_body("<script>\nlet s = \"</script>\"\n"), None);
    }
}
