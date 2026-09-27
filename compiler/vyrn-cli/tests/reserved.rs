//! A top-level declaration may not take a name the compiler owns, and saying so
//! must not depend on what the program links.
//!
//! A reserved name in the loader's `owner` map made every use of the builtin in
//! a linked `std/` module look like an unimported foreign reference: `fn at`
//! plus one `print` gave 53 diagnostics in `std/num.vyrn`, none at the
//! declaration.

use std::process::Command;

/// `slot` names a directory of this test's own: tests in one binary run in
/// parallel.
fn check_in(slot: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!("vyrn-reserved-{slot}"));
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

/// A count alone would not catch a flood that happened to be shorter, so the
/// first diagnostic must name the reserved word in the user's file.
#[test]
fn a_reserved_top_level_name_is_reported_once_at_its_declaration() {
    // `lineAt` witnesses a routed builtin.
    for name in ["at", "push", "len", "pop", "toString", "lineAt"] {
        // The `print` links `std/num`, and linking a std module that uses the
        // builtin is what triggers the flood.
        let src = format!(
            "fn {name}(v: Int64) -> Int64 {{ return v }}\n\
             fn main() -> Int64 {{ print({name}(1)) return 0 }}\n"
        );
        let got = check_in(name, &src);
        let first = got.lines().next().unwrap_or("");
        assert!(
            first.contains(&format!("`{name}` is a reserved name")),
            "`{name}`: first diagnostic should name it reserved, got:\n{got}"
        );
        assert!(
            !got.contains("std/num.vyrn") && !got.contains("std/strpred.vyrn"),
            "`{name}`: diagnostics must stay in the user's file, got:\n{got}"
        );
        // One cascade is correct: the call resolves to the builtin, whose arity
        // differs.
        assert!(
            got.lines().filter(|l| !l.trim().is_empty()).count() <= 2,
            "`{name}`: expected at most 2 diagnostics, got:\n{got}"
        );
    }
}

/// A name the compiler gave back may be declared, and the declaration wins.
/// The `print` links a std module here too, so a flood would show.
#[test]
fn a_name_the_compiler_gave_back_may_be_declared() {
    for name in ["slice", "contains", "chars", "hexEncode"] {
        let src = format!(
            "fn {name}(v: Int64) -> Int64 {{ return v }}\n\
             fn main() -> Int64 {{ print({name}(1)) return 0 }}\n"
        );
        let got = check_in(name, &src);
        assert_eq!(got.trim(), "ok", "`{name}` must be declarable, got:\n{got}");
    }
}
