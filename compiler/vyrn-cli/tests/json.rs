//! Integration test for `std/json` (RFC-0059): the module's own inline unit
//! suite — strict-parse rejections with pinned `line N, col M` wording, the full
//! escape set including surrogate pairs, the round-trip law, and field-order
//! preservation — runs green through the real `vyrn` binary.

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
