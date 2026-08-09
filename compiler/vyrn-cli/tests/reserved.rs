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

/// `slot` names a directory of this test's own: two tests run in parallel in one
/// binary, and one file for both is one file being rewritten under the other.
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

/// The regression itself: the diagnostic must name the reserved word, sit at the
/// declaration, and be in the user's file. A count alone would not catch a
/// return to 53 messages that merely happened to be shorter.
#[test]
fn a_reserved_top_level_name_is_reported_once_at_its_declaration() {
    // `get`, `set`, `cell` and `release` left this list with Path B (RFC-0090 M4).
    // A user owns those four names now, which is the deletion visible from the
    // surface: `fn get(..)` compiles. RFC-0094 M2 did the same for eleven more,
    // `slice` among them — see `a_name_returned_by_m2_may_be_declared` below.
    for name in ["at", "push", "len", "pop", "toString"] {
        // The `print` is load-bearing: it is what links `std/num`, and linking a
        // std module that uses the builtin is what used to trigger the flood.
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
        // One cascade is allowed and correct — the call now resolves to the
        // builtin, whose arity genuinely differs — but nothing beyond it.
        assert!(
            got.lines().filter(|l| !l.trim().is_empty()).count() <= 2,
            "`{name}`: expected at most 2 diagnostics, got:\n{got}"
        );
    }
}

/// The other half of the same rule, and RFC-0094 M2's visible deletion: a name
/// the compiler gave back may be declared, and the declaration wins.
///
/// The flood this file exists to prevent came from a reserved name entering the
/// loader's `owner` map. A name that is no longer reserved must not produce one
/// either — so the `print` stays load-bearing here for the same reason.
#[test]
fn a_name_returned_by_m2_may_be_declared() {
    for name in ["slice", "contains", "chars", "hexEncode"] {
        let src = format!(
            "fn {name}(v: Int64) -> Int64 {{ return v }}\n\
             fn main() -> Int64 {{ print({name}(1)) return 0 }}\n"
        );
        let got = check_in(name, &src);
        assert_eq!(got.trim(), "ok", "`{name}` must be declarable, got:\n{got}");
    }
}
