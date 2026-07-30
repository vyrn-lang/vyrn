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
    assert!(combined.contains(expected), "expected `{expected}`:\n{combined}");
}

#[test]
fn std_json_writer_unit_tests_run_green() {
    unit_tests_green("std/json.vyrn", "2 passed, 0 failed");
}

#[test]
fn std_jsonread_unit_tests_run_green() {
    unit_tests_green("std/jsonread.vyrn", "13 passed, 0 failed");
}

#[test]
fn tojson_byte_pins_hold() {
    let example = repo_file("examples/jsonbytes.vyrn");
    let out = vyrn().arg("test").arg(&example).output().expect("vyrn test");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "toJson byte pins failed:\n{combined}");
    assert!(combined.contains("7 passed, 0 failed"), "expected 7 green pins:\n{combined}");
}
