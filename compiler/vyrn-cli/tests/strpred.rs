//! The string tier's equivalence proof (RFC-0078 M4b).
//!
//! `std/strpred` writes `contains`, `startsWith`, `endsWith`, `slice` and
//! `byteLength` — five builtins with three implementations each — as ordinary
//! Vyrn on the byte view. Nothing is swapped yet, which is exactly why this
//! exists: equivalence has to be proved BEFORE a deletion, or the test ends up
//! describing whatever the new code happens to do.
//!
//! The oracle is the builtin itself. `examples/strpredbytes.vyrn` calls both in
//! the same program over the surface where two substring searches can differ, and
//! its `test` blocks assert they agree — parity already runs the example's `main`
//! (so interp == native == wasm is covered), but nothing else in the suite runs an
//! example's `test` blocks, so without the first row here the assertions are
//! decoration.
//!
//! The rest of the file covers the one thing a single program cannot: `slice`
//! **traps**, and a trap ends the process. `sliceV` returns `Option<String>`
//! instead, because Vyrn has no expression that aborts with a message. That makes
//! "`None` exactly where the builtin traps" the property to check, and it needs
//! one process per case — each program prints `sliceV`'s answer to stdout and THEN
//! calls the builtin on the same range, so a single run pins both halves of the
//! pairing and the canonical wording with it.

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
fn string_predicate_equivalence_pins_hold() {
    let module = repo_file("examples/strpredbytes.vyrn");
    let out = vyrn().arg("test").arg(&module).output().expect("vyrn test");
    let combined = norm(&out.stdout) + &norm(&out.stderr);
    assert!(out.status.success(), "strpredbytes unit tests failed:\n{combined}");
    assert!(combined.contains("5 passed, 0 failed"), "expected 5 green:\n{combined}");
}

/// One case: `sliceV` must print `None` and the builtin must then trap with
/// `expect`, in the same process, on the same range.
fn slice_trap_pairs_with_none(name: &str, literal: &str, start: &str, end: &str, expect: &str) {
    let dir = std::env::temp_dir().join("vyrn-strpred");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.vyrn"));
    let src = format!(
        "import {{ sliceV }} from \"std/strpred\"\n\
         fn main() -> Int64 {{\n\
         \x20   print(match sliceV(\"{literal}\", {start}, {end}) {{ Some(v) => v, None => \"None\" }})\n\
         \x20   print(slice(\"{literal}\", {start}, {end}))\n\
         \x20   return 0\n\
         }}\n"
    );
    std::fs::write(&path, src).unwrap();

    let out = vyrn().arg("run").arg(&path).output().expect("vyrn run");
    let (stdout, stderr) = (norm(&out.stdout), norm(&out.stderr));
    assert_eq!(
        stdout, "None\n",
        "{name}: sliceV must decline where the builtin traps (stderr: {stderr:?})"
    );
    assert_eq!(stderr, format!("{expect}\n"), "{name}: trap wording");
    assert_eq!(out.status.code(), Some(1), "{name}: a trap exits 1");
}

/// `slice`'s FIRST trap: the range itself. Checked before the UTF-8 boundary on
/// every engine, which is why a negative start on a multi-byte string reports
/// out-of-range rather than a split character.
#[test]
fn slice_out_of_range_traps_and_slicev_declines() {
    let m = "error: slice index out of range";
    slice_trap_pairs_with_none("oob_start_gt_end", "hello", "3", "2", m);
    slice_trap_pairs_with_none("oob_end_past_len", "hello", "0", "6", m);
    slice_trap_pairs_with_none("oob_start_past_len", "hello", "6", "6", m);
    slice_trap_pairs_with_none("oob_negative_start", "hello", "-1", "2", m);
    // A negative end is caught by `start > end` rather than by a lower bound, so
    // it reports the same message — worth pinning, since it is the one shape the
    // condition reaches by a different clause.
    slice_trap_pairs_with_none("oob_negative_end", "hello", "0", "-1", m);
    // The range is checked first even when the offsets are also mid-character.
    slice_trap_pairs_with_none("oob_beats_split", "héllo", "2", "99", m);
}

/// `slice`'s SECOND trap: a cut inside a multi-byte character. The answer to
/// "does it trap, or produce invalid UTF-8?" is that it traps — the builtin
/// refuses the range, and `stringFromBytes` refuses the same bytes, which is how
/// `sliceV` gets the boundary check without writing one.
#[test]
fn slice_mid_codepoint_traps_and_slicev_declines() {
    let m = "error: slice splits a UTF-8 character";
    slice_trap_pairs_with_none("split_end_in_2byte", "héllo", "0", "2", m);
    slice_trap_pairs_with_none("split_start_in_2byte", "héllo", "2", "6", m);
    slice_trap_pairs_with_none("split_both_in_3byte", "日本語", "1", "2", m);
    slice_trap_pairs_with_none("split_in_4byte", "😀ok", "0", "2", m);
}
