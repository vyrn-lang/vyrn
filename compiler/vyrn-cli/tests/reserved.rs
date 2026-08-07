//! A top-level declaration may not take a name the compiler owns, and saying so
//! must not depend on what the program happens to link.
//!
//! The loader builds a flat `owner` map of every top-level name. A reserved name
//! entering it made every use of the BUILTIN inside a linked `std/` module look
//! like an unimported foreign reference — so `fn at` plus one `print` produced
//! **53** diagnostics, all pointing into `std/num.vyrn`, none at the declaration
//! and none saying the name was reserved. The count tracked how often that std
//! module used the builtin (`at` 53, `push` 23, `slice` 2), which is why it was
//! never noticed: without a call nothing links, and the checker's own guard
//! reported correctly.

use std::process::Command;

fn check(src: &str) -> String {
    let dir = std::env::temp_dir().join("vyrn-reserved");
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("r.vyrn");
    std::fs::write(&f, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_vyrn"))
        .arg("check")
        .arg(&f)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n")
        + &String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
}

/// The regression itself: the diagnostic must name the reserved word, sit at the
/// declaration, and be in the user's file. A count alone would not catch a
/// return to 53 messages that merely happened to be shorter.
#[test]
fn a_reserved_top_level_name_is_reported_once_at_its_declaration() {
    // `get`, `set`, `cell` and `release` left this list with Path B (RFC-0090 M4).
    // A user owns those four names now, which is the deletion visible from the
    // surface: `fn get(..)` compiles.
    for name in ["at", "push", "slice", "len", "pop", "toString"] {
        // The `print` is load-bearing: it is what links `std/num`, and linking a
        // std module that uses the builtin is what used to trigger the flood.
        let src = format!(
            "fn {name}(v: Int64) -> Int64 {{ return v }}\n\
             fn main() -> Int64 {{ print({name}(1)) return 0 }}\n"
        );
        let got = check(&src);
        let first = got.lines().next().unwrap_or("");
        assert!(
            first.contains(&format!("`{name}` is a reserved name")),
            "`{name}`: first diagnostic should name it reserved, got:\n{got}"
        );
        assert!(
            !got.contains("std/num.vyrn") && !got.contains("std/strpred.vyrn"),
            "`{name}`: diagnostics must stay in the user's file, got:\n{got}"
        );
        // One cascade is allowed and correct — the call now resolves to the
        // builtin, whose arity genuinely differs — but nothing beyond it.
        assert!(
            got.lines().filter(|l| !l.trim().is_empty()).count() <= 2,
            "`{name}`: expected at most 2 diagnostics, got:\n{got}"
        );
    }
}
