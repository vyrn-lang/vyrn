//! Integration tests for JSON's two writers.
//!
//! `std/json` (RFC-0059): the module's own inline unit suite — strict-parse
//! rejections with pinned `line N, col M` wording, the full escape set including
//! surrogate pairs, the round-trip law, and field-order preservation — runs green
//! through the real `vyrn` binary.
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

#[test]
fn std_json_unit_tests_run_green() {
    let module = repo_file("std/json.vyrn");
    let out = vyrn().arg("test").arg(&module).output().expect("vyrn test");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "std/json unit tests failed:\n{combined}");
    assert!(combined.contains("15 passed, 0 failed"), "expected 15 green tests:\n{combined}");
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
