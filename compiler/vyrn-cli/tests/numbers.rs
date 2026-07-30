//! The number tier's pins (RFC-0078 M4a).
//!
//! `examples/numbytes.vyrn` captures every numeric conversion the language has —
//! text -> `Int64` including each way it declines, `%f`'s six places on the values
//! that tell an exact formatter from a plausible one, the saturating float ->
//! integer conversions and the wrapping narrowings — as literals in `test` blocks,
//! BEFORE any of it moves out of Rust and C and into Vyrn.
//!
//! Parity already runs the example's `main`, so interp == native == wasm is
//! covered without this file. What is not covered is the `test` blocks: nothing
//! else in the suite runs an example's, so without this row the pins are
//! decoration.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_file(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel).canonicalize().unwrap()
}

fn vyrn() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vyrn"))
}

/// Run one module's inline `test` blocks and assert the green count.
fn unit_tests_green(rel: &str, expected: &str) {
    let module = repo_file(rel);
    let out = vyrn().arg("test").arg(&module).output().expect("vyrn test");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{rel} unit tests failed:\n{combined}");
    assert!(combined.contains(expected), "expected `{expected}`:\n{combined}");
}

#[test]
fn number_conversion_pins_hold() {
    unit_tests_green("examples/numbytes.vyrn", "7 passed, 0 failed");
}
