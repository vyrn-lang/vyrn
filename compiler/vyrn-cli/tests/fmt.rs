//! `vyrn fmt` integration tests (RFC-0017), focused on the `--check` CI gate
//! and the CRLF line-ending policy. Interpreter-agnostic (no clang), so these
//! run in the default suite.
//!
//! CRLF policy: `fmt` preserves a file's existing line-ending style — a CRLF
//! (Windows-authored) file round-trips to CRLF, an LF file to LF. So a
//! canonically-formatted CRLF file is NOT a spurious diff under `--check`, and
//! `fmt` never rewrites a whole file just to flip its newlines.

use std::path::PathBuf;
use std::process::Command;

fn vyrn() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vyrn"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("vyrn-fmt-tests").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A canonically-formatted program (matches `vyrn fmt` output) with LF endings.
const CANON_LF: &str = "fn main() -> Int64 {\n    let x = if true { 1 } else { 2 }\n    return x\n}\n";

#[test]
fn check_passes_on_an_already_formatted_lf_file() {
    let dir = scratch("canon-lf");
    let file = dir.join("a.vyrn");
    std::fs::write(&file, CANON_LF).unwrap();
    let out = vyrn().arg("fmt").arg("--check").arg(&file).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    // No files listed as "would change".
    assert!(out.stdout.is_empty(), "stdout: {}", String::from_utf8_lossy(&out.stdout));
    // Untouched.
    assert_eq!(std::fs::read_to_string(&file).unwrap(), CANON_LF);
}

#[test]
fn check_flags_a_misformatted_file_without_writing() {
    let dir = scratch("misformatted");
    let file = dir.join("bad.vyrn");
    let messy = "fn  main()->Int64{\nlet   x=1\nreturn x\n}\n";
    std::fs::write(&file, messy).unwrap();
    let out = vyrn().arg("fmt").arg("--check").arg(&file).output().unwrap();
    // Exit nonzero and the path is listed.
    assert_eq!(out.status.code(), Some(1));
    let listed = String::from_utf8_lossy(&out.stdout);
    assert!(listed.contains("bad.vyrn"), "stdout: {listed}");
    // --check writes nothing.
    assert_eq!(std::fs::read_to_string(&file).unwrap(), messy);
}

#[test]
fn check_does_not_flag_an_already_formatted_crlf_file() {
    // The heart of the CRLF policy: a Windows-authored file that is otherwise
    // canonical must round-trip without a spurious diff.
    let dir = scratch("canon-crlf");
    let file = dir.join("crlf.vyrn");
    let canon_crlf = CANON_LF.replace('\n', "\r\n");
    std::fs::write(&file, &canon_crlf).unwrap();
    let out = vyrn().arg("fmt").arg("--check").arg(&file).output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "CRLF file spuriously flagged; stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    // Byte-for-byte untouched (still CRLF).
    assert_eq!(std::fs::read(&file).unwrap(), canon_crlf.as_bytes());
}

#[test]
fn write_preserves_crlf_endings() {
    // Formatting a misformatted CRLF file rewrites it to canonical form but keeps
    // CRLF endings (never silently converts to LF).
    let dir = scratch("write-crlf");
    let file = dir.join("w.vyrn");
    let messy_crlf = "fn  main()->Int64{\r\nlet   x=1\r\nreturn x\r\n}\r\n";
    std::fs::write(&file, messy_crlf).unwrap();
    let out = vyrn().arg("fmt").arg(&file).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let after = std::fs::read_to_string(&file).unwrap();
    // Every line ends CRLF, none bare-LF.
    assert!(after.contains("\r\n"), "expected CRLF endings, got: {after:?}");
    assert!(!after.replace("\r\n", "").contains('\n'), "found a bare LF: {after:?}");
    // And it is now canonical: a second --check passes.
    let recheck = vyrn().arg("fmt").arg("--check").arg(&file).output().unwrap();
    assert_eq!(recheck.status.code(), Some(0), "not idempotent under CRLF");
}
