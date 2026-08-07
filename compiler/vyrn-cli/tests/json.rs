//! Integration tests for JSON's two writers.
//!
//! `std/json` + `std/jsonread` (RFC-0059, split by RFC-0078 M2a): the two
//! modules' own inline unit suites — the writer's escaping and indentation on one
//! side, and on the other the strict-parse rejections with pinned `line N, col M`
//! wording, the full escape set including surrogate pairs, the round-trip law and
//! field-order preservation — run green through the real `vyrn` binary. Two rows
//! rather than one because `vyrn test` runs one module's blocks, and the point of
//! the split is that the writer links without the reader.
//!
//! `examples/jsonbytes.vyrn` (RFC-0078 M2): the bytes `toJson` produces, pinned as
//! literals BEFORE its serializer half is rewired from the C shim to `std/json`.
//! The example carries the pins in `test` blocks, and nothing else in the suite
//! runs an example's `test` blocks, so without this row the pins are decoration.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_file(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
        .canonicalize()
        .unwrap()
}

fn vyrn() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vyrn"))
}

/// Run one std module's inline `test` blocks and assert the green count.
fn unit_tests_green(rel: &str, expected: &str) {
    let module = repo_file(rel);
    let out = vyrn().arg("test").arg(&module).output().expect("vyrn test");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{rel} unit tests failed:\n{combined}");
    assert!(
        combined.contains(expected),
        "expected `{expected}`:\n{combined}"
    );
}

#[test]
fn std_json_writer_unit_tests_run_green() {
    unit_tests_green("std/json.vyrn", "2 passed, 0 failed");
}

#[test]
fn std_jsonread_unit_tests_run_green() {
    unit_tests_green("std/jsonread.vyrn", "13 passed, 0 failed");
}

/// RFC-0078 M2b: the injected import is invisible. A program that mentions
/// `toJson` links `std/json` without saying so, and that module's declarations
/// must be unable to collide with the program's own or to capture the desugar's
/// calls.
///
/// Every name in here is one `std/json` also declares — `emit`, `hex2`,
/// `emitString`, the type `Json`, and an enum variant `JStr`, which is the one that
/// bites hardest: before the reserved spellings, a variant-name clash with a module
/// in the link was rejected outright ("function `JStr` is defined in
/// `std/json.vyrn` but not imported here"), so injection would have turned a legal
/// program into an error naming a module the user never mentioned.
#[test]
fn an_injected_runtime_module_cannot_collide_with_the_users_names() {
    let dir = std::env::temp_dir().join("vyrn-m2b-inject");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("collide.vyrn");
    std::fs::write(
        &file,
        "type Json = | Mine | JStr(String)\n\
         type P = { n: Int64 }\n\
         fn emit(x: Int64) -> String { return \"user emit \" + x.toString() }\n\
         fn hex2(b: Int64) -> String { return \"user hex2\" }\n\
         fn emitString(s: String) -> String { return \"user emitString\" }\n\
         fn main() -> Int64 {\n\
         print(emit(7))\n\
         print(hex2(3))\n\
         print(emitString(\"q\"))\n\
         print(match JStr(\"v\") { Mine => \"mine\", JStr(s) => s })\n\
         print(toJson(P { n: 5 }))\n\
         return 0\n\
         }\n",
    )
    .unwrap();
    let out = vyrn().arg("run").arg(&file).output().expect("vyrn run");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "collision program failed:\n{combined}"
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
        "user emit 7\nuser hex2\nuser emitString\nv\n{\"n\":5}\n",
        "the user's own names must win, and `toJson` must still work:\n{combined}"
    );
}

#[test]
fn tojson_byte_pins_hold() {
    let example = repo_file("examples/jsonbytes.vyrn");
    let out = vyrn()
        .arg("test")
        .arg(&example)
        .output()
        .expect("vyrn test");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "toJson byte pins failed:\n{combined}");
    assert!(
        combined.contains("7 passed, 0 failed"),
        "expected 7 green pins:\n{combined}"
    );
}

/// `toJson` was O(N²) in array length — 40k `Int64` took 2.5 s, 80k took 23.5 s,
/// and 50k three-float records ran the machine out of memory producing 2.5 MB of
/// JSON. All of it was in `std/json`'s writer: `emitArr`/`emitObj` end with
/// `return out + "]"`, and codegen's in-place String append banned any name that
/// appeared under a `+`, so every element re-`malloc`'d and re-copied the whole
/// result so far and leaked the previous buffer.
///
/// The pin is structural — a COUNT of copying concatenations, not a duration, so
/// it cannot go flaky on a loaded machine. Each writer may copy exactly once, in
/// its tail; a second `strcat` means the per-element append went back to
/// allocating, which is the complexity class regressing. (`vyrn-codegen`'s
/// `accumulator_returned_through_a_concat_still_appends_in_place` pins the
/// compiler rule; this pins that `std/json` is actually written in the shape the
/// rule recognizes, which is the half a library edit could silently undo.)
#[test]
fn the_json_writer_does_not_copy_once_per_element() {
    let dir = std::env::temp_dir().join("vyrn-json-linear");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("linear.vyrn");
    std::fs::write(
        &file,
        "fn main() -> Int64 {\n\
         let mut a: Array<Int64> = []\n\
         a.push(1)\n\
         print(toJson(a).byteLength)\n\
         return 0\n\
         }\n",
    )
    .unwrap();
    let out = vyrn()
        .arg("emit-ir")
        .arg(&file)
        .output()
        .expect("vyrn emit-ir");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let ir = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    for writer in ["vyrn_json$emitArr", "vyrn_json$emitObj"] {
        let start = ir
            .find(&format!("define ptr @{writer}("))
            .unwrap_or_else(|| panic!("no `{writer}` in the emitted IR"));
        let body = &ir[start..start + ir[start..].find("\n}\n").expect("unterminated body")];
        assert!(
            body.contains("call ptr @__vyrn_realloc"),
            "`{writer}` must grow its accumulator in place:\n{body}"
        );
        assert_eq!(
            body.matches("call ptr @__vyrn_str_concat(").count(),
            1,
            "`{writer}` may copy only in its tail — a second copy is one per \
             element, which is the O(N²) `toJson` had:\n{body}"
        );
    }
}

/// The same, for the other direction (RFC-0078 M3): `examples/jsondecbytes.vyrn`
/// pins what `fromJson` produces — weighted towards the accumulated `Issue`s,
/// their order, and the parse-error wording, which is where two READERS differ and
/// which nothing else in the suite looks at.
///
/// One of its blocks pins three rows where `std/jsonread` reads the same input
/// differently, two of them changing which documents parse at all. That block is
/// the evidence M3 is a semantic ruling rather than a byte-formatting one, so it
/// has to fail loudly if either reader moves under it.
#[test]
fn fromjson_byte_pins_hold() {
    let example = repo_file("examples/jsondecbytes.vyrn");
    let out = vyrn()
        .arg("test")
        .arg(&example)
        .output()
        .expect("vyrn test");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "fromJson byte pins failed:\n{combined}"
    );
    assert!(
        combined.contains("10 passed, 0 failed"),
        "expected 10 green pins:\n{combined}"
    );
}
