//! `std/cli` — the command line is a record type (RFC-0098 M1).
//!
//! Two rows, and neither duplicates the parity harness. Parity proves the two
//! examples say the same bytes on interp, native and wasm; it is the `--ignored`
//! gate, so a plain `cargo test --workspace` never runs it. These rows run by
//! default, and what they pin is what parity cannot: that the library's inline
//! tests are green, and that a refused argv still says the library's own words.
//!
//! The wording is pinned here rather than left to parity because parity compares
//! the three engines against EACH OTHER. Three engines agreeing on a message that
//! leaked an internal name would be green.

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

/// The runtime half and the comptime helpers, tested over plain arrays with no
/// generation involved.
#[test]
fn std_cli_unit_tests_run_green() {
    let module = repo_file("std/cli.vyrn");
    let out = vyrn().arg("test").arg(&module).output().expect("vyrn test");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "std/cli unit tests failed:\n{combined}"
    );
    assert!(
        combined.contains("10 passed, 0 failed"),
        "expected `10 passed, 0 failed`:\n{combined}"
    );
}

/// One argv breaking four rules at once, read out of the example's own `.args`
/// fixture so this row and the parity row can never drift apart.
///
/// The assertions are on the WORDS. Every message names what the user typed —
/// `--roott`, `--port` — and the field it belongs to; none names a generated
/// local, a spec index or a synthesized module.
#[test]
fn a_refused_argv_says_the_librarys_own_words() {
    let dir = repo_file("examples");
    let fixture = std::fs::read_to_string(dir.join("clifail.args")).unwrap();
    let argv: Vec<&str> = fixture.lines().filter(|l| !l.is_empty()).collect();

    let out = vyrn()
        .arg("run")
        .arg("clifail.vyrn")
        .args(&argv)
        .current_dir(&dir)
        .output()
        .expect("vyrn run");

    assert_eq!(out.status.code(), Some(2), "a usage error exits 2");
    let stderr = String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n");
    let stdout = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");

    // `Validation` accumulates: four broken rules arrive together, not one.
    for want in [
        "unknown option `--roott`",
        "verbose: option `--verbose` takes no value",
        "port: option `--port` expects a whole number, 1 to 65535",
        "root: required option `--root` is missing",
    ] {
        assert!(stderr.contains(want), "missing `{want}`:\n{stderr}");
    }

    // The help is the same declaration's other output, and the option prose is
    // the `///` above each option's own named type.
    assert!(
        stdout.contains("Serve a directory over HTTP."),
        "the record's own doc is the summary:\n{stdout}"
    );
    assert!(
        stdout.contains("-p, --port <value>  The TCP port to listen on. (1 to 65535)"),
        "the option's type carries its help and its bound:\n{stdout}"
    );

    // No unspellable internal name reaches a user. `@` prefixes the desugared
    // names, `$` the injected ones, and `__vyrn_` the host symbols.
    for leak in ["@", "$", "__vyrn_", "TypeInfo", "moduleInterface"] {
        assert!(
            !stderr.contains(leak),
            "an internal name reached the user (`{leak}`):\n{stderr}"
        );
    }
}
