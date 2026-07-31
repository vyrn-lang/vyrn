//! The string tier (RFC-0078 M4b(3), converted by M4c and RFC-0079 M3).
//!
//! `std/strpred` writes `contains`, `startsWith`, `endsWith`, `slice` and
//! `byteLength` as ordinary Vyrn on the byte view. **Four of the five moved and
//! one did not**, which is what this file's shape now reflects.
//!
//! - `contains` / `startsWith` / `endsWith` / `slice` are routed into the module,
//!   so the example's rows are **literals** captured from the C and Rust
//!   implementations before the swap. A test that still said "the two agree" would
//!   compare a function with itself.
//! - `byteLength` is a VIEW (`strlen`) that folds at compile time inside refinement
//!   predicates, so it stayed a builtin and keeps a live oracle — the example
//!   prints the builtin's answer beside the Vyrn one and asserts they agree.
//!
//! **This file used to be mostly a trap harness, and RFC-0079 M3 deleted that
//! half.** `slice` trapped, and a trap ends the process, so "`None` exactly where
//! the builtin traps" needed one PROCESS PER CASE: fourteen programs, each printing
//! `sliceV`'s answer and then calling the builtin on the same range, with the
//! canonical wording asserted from the parent. `slice` returns its failure now, so
//! all fourteen ranges are ordinary printed values inside
//! `examples/strpredbytes.vyrn` — which means they go through the three-engine
//! parity corpus, which the trapping programs never could. What is left here is the
//! one thing parity does not do: run the example's `test` blocks.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_file(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel).canonicalize().unwrap()
}

fn vyrn() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vyrn"))
}

fn norm(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace("\r\n", "\n")
}

#[test]
fn string_predicate_pins_hold() {
    let module = repo_file("examples/strpredbytes.vyrn");
    let out = vyrn().arg("test").arg(&module).output().expect("vyrn test");
    let combined = norm(&out.stdout) + &norm(&out.stderr);
    assert!(out.status.success(), "strpredbytes unit tests failed:\n{combined}");
    // Four, not five: M4c deleted the `predsAgree` block, which after the routing
    // asserted that a function equals itself over twenty inputs. Its rows became
    // the literal table in the block that replaced it. M3 kept the count — the
    // `sliceV`-declines block became "slice names which cut failed and at which
    // byte", which is a stronger statement over the same fourteen ranges.
    assert!(combined.contains("4 passed, 0 failed"), "expected 4 green:\n{combined}");
}

/// `std/strings`'s `substring` is the ONE place in `std/` that turns a `SliceError`
/// into a crash, and this is the wording pin the two deleted `@.trap.slice*`
/// globals used to hold.
///
/// It is here rather than in the example because it still ends the process, and it
/// is worth pinning because the message moved from the compiler to a library: the
/// catalogue said `error: slice index out of range` with no way to name the index,
/// and this says which offset and why. `?? panic` in a library is deterministic
/// text, so parity still compares it byte for byte — `nullish_and_panic_say_the_
/// same_bytes_on_all_three_engines` is the three-engine half of this.
#[test]
fn substring_names_the_offset_it_refused() {
    let dir = std::env::temp_dir().join("vyrn-strpred");
    std::fs::create_dir_all(&dir).unwrap();
    for (name, literal, start, end, expect) in [
        (
            "oob_start_gt_end",
            "hello",
            "3",
            "2",
            "error: substring: byte offset 2 is out of range for a String of 5 bytes",
        ),
        (
            "oob_end_past_len",
            "hello",
            "0",
            "6",
            "error: substring: byte offset 6 is out of range for a String of 5 bytes",
        ),
        (
            "oob_negative_start",
            "hello",
            "-1",
            "2",
            "error: substring: byte offset -1 is out of range for a String of 5 bytes",
        ),
        (
            "split_end_in_2byte",
            "héllo",
            "0",
            "2",
            "error: substring: byte offset 2 is inside a multi-byte UTF-8 character",
        ),
        (
            "split_both_in_3byte",
            "日本語",
            "1",
            "2",
            "error: substring: byte offset 1 is inside a multi-byte UTF-8 character",
        ),
    ] {
        let path = dir.join(format!("{name}.vyrn"));
        let src = format!(
            "import {{ substring }} from \"std/strings\"\n\
             fn main() -> Int64 {{\n\
             \x20   print(\"before\")\n\
             \x20   print(substring(\"{literal}\", {start}, {end}))\n\
             \x20   return 0\n\
             }}\n"
        );
        std::fs::write(&path, src).unwrap();
        let out = vyrn().arg("run").arg(&path).output().expect("vyrn run");
        assert_eq!(norm(&out.stdout), "before\n", "{name}: stdout before the panic");
        assert_eq!(norm(&out.stderr), format!("{expect}\n"), "{name}: panic wording");
        assert_eq!(out.status.code(), Some(1), "{name}: a panic exits 1");
    }
}
