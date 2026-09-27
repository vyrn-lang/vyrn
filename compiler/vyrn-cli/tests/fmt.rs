//! `vyrn fmt`: the `--check` gate and the line-ending policy. `fmt` keeps a
//! file's line endings, CRLF or LF, so a canonical CRLF file is no diff under `--check`
//! and `fmt` never rewrites a whole file to flip its newlines. No clang needed.

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
const CANON_LF: &str =
    "fn main() -> Int64 {\n    let x = if true { 1 } else { 2 }\n    return x\n}\n";

#[test]
fn check_passes_on_an_already_formatted_lf_file() {
    let dir = scratch("canon-lf");
    let file = dir.join("a.vyrn");
    std::fs::write(&file, CANON_LF).unwrap();
    let out = vyrn()
        .arg("fmt")
        .arg("--check")
        .arg(&file)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), CANON_LF);
}

#[test]
fn check_flags_a_misformatted_file_without_writing() {
    let dir = scratch("misformatted");
    let file = dir.join("bad.vyrn");
    let messy = "fn  main()->Int64{\nlet   x=1\nreturn x\n}\n";
    std::fs::write(&file, messy).unwrap();
    let out = vyrn()
        .arg("fmt")
        .arg("--check")
        .arg(&file)
        .output()
        .unwrap();
    // Exit nonzero and the path is listed.
    assert_eq!(out.status.code(), Some(1));
    let listed = String::from_utf8_lossy(&out.stdout);
    assert!(listed.contains("bad.vyrn"), "stdout: {listed}");
    // --check writes nothing.
    assert_eq!(std::fs::read_to_string(&file).unwrap(), messy);
}

#[test]
fn check_does_not_flag_an_already_formatted_crlf_file() {
    // A Windows-authored file that is otherwise canonical round-trips with no diff.
    let dir = scratch("canon-crlf");
    let file = dir.join("crlf.vyrn");
    let canon_crlf = CANON_LF.replace('\n', "\r\n");
    std::fs::write(&file, &canon_crlf).unwrap();
    let out = vyrn()
        .arg("fmt")
        .arg("--check")
        .arg(&file)
        .output()
        .unwrap();
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
    // A misformatted CRLF file is rewritten to canonical form and keeps CRLF endings.
    let dir = scratch("write-crlf");
    let file = dir.join("w.vyrn");
    let messy_crlf = "fn  main()->Int64{\r\nlet   x=1\r\nreturn x\r\n}\r\n";
    std::fs::write(&file, messy_crlf).unwrap();
    let out = vyrn().arg("fmt").arg(&file).output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let after = std::fs::read_to_string(&file).unwrap();
    // Every line ends CRLF, none bare-LF.
    assert!(
        after.contains("\r\n"),
        "expected CRLF endings, got: {after:?}"
    );
    assert!(
        !after.replace("\r\n", "").contains('\n'),
        "found a bare LF: {after:?}"
    );
    // And it is canonical: a second --check passes.
    let recheck = vyrn()
        .arg("fmt")
        .arg("--check")
        .arg(&file)
        .output()
        .unwrap();
    assert_eq!(recheck.status.code(), Some(0), "not idempotent under CRLF");
}

#[test]
fn a_vyx_component_is_left_alone() {
    // `fmt` refuses a `.vyx` file: lexed as a Vyrn module, its template gains spaces
    // inside every tag and sentence (`< / p >`), and the re-lex invariant does not catch
    // it because the mangled text re-lexes to the same tokens.
    let dir = scratch("vyx-skip");
    let file = dir.join("Card.vyx");
    let source = "<script>\nfn  label()->String{return \"x\"}\n</script>\n\n<template>\n<p class=\"a\">One program. Three backends.</p>\n</template>\n";
    std::fs::write(&file, source).unwrap();

    let out = vyrn().arg("fmt").arg(&file).output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        source,
        "fmt rewrote a .vyx template"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("cannot format a .vyx"),
        "expected a note naming the skip, got: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // `--check` says nothing about a file it will not format, so a project with
    // components still passes the gate.
    let check = vyrn()
        .arg("fmt")
        .arg("--check")
        .arg(&file)
        .output()
        .unwrap();
    assert_eq!(check.status.code(), Some(0));
}

/// A brace opened inside an open bracket is a continuation of the call, not a second step
/// in. `print(match e { .. })` is the shape: counting brackets puts the arms two levels in
/// and the `})` one.
#[test]
fn a_brace_inside_an_open_bracket_indents_one_level() {
    let dir = scratch("brace-in-bracket");
    let file = dir.join("a.vyrn");
    let src =
        "fn main() -> Int64 {\n    print(match 1 {\n        _ => \"x\",\n    })\n    return 0\n}\n";
    std::fs::write(&file, src).unwrap();
    let out = vyrn()
        .arg("fmt")
        .arg("--check")
        .arg(&file)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), src);
}
