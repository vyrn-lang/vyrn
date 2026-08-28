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

/// `vyrn --version` and `vyrn -V`, and the three facts about them a package
/// manager reads: exit 0, one line on stdout, and the crate's own version in it.
///
/// The published alpha (v0.1.0-alpha.1) printed the usage screen and exited 2
/// for both spellings. The version comes from `CARGO_PKG_VERSION`, so this row
/// also pins that the string is not a second copy of the number.
#[test]
fn version_prints_the_crate_version_and_exits_zero() {
    for flag in ["--version", "-V"] {
        let out = vyrn().arg(flag).output().expect("vyrn --version");
        assert_eq!(out.status.code(), Some(0), "`vyrn {flag}` exits 0");
        let stdout = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
        assert_eq!(
            stdout,
            format!("vyrn {}\n", env!("CARGO_PKG_VERSION")),
            "`vyrn {flag}` prints one line, and it is the crate's version"
        );
    }
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

/// `vyrn run --profile` prints the table to STDERR and leaves stdout alone.
///
/// Both halves matter. A run whose stdout is piped somewhere must pipe the same
/// bytes with the flag as without it, and the flag belongs to the CLI rather
/// than the program — so it counts only before the file, and `vyrn run app.vyrn
/// --profile` hands it to `app.vyrn` as an ordinary argument.
#[test]
fn profile_writes_the_table_to_stderr_and_not_to_stdout() {
    let dir = std::env::temp_dir().join("vyrn-cli-profile");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("p.vyrn");
    std::fs::write(
        &file,
        "fn work(n: Int64) -> Int64 {\n\
         \x20   let mut h = 0\n\
         \x20   let mut i = 0\n\
         \x20   while i < n { h = h + i  i = i + 1 }\n\
         \x20   return h\n\
         }\n\
         fn main() -> Int64 {\n\
         \x20   let a = args()\n\
         \x20   print(work(1000) + a.length)\n\
         \x20   return 0\n\
         }\n",
    )
    .unwrap();

    let plain = Command::new(env!("CARGO_BIN_EXE_vyrn"))
        .arg("run")
        .arg(&file)
        .output()
        .expect("vyrn run");
    let profiled = Command::new(env!("CARGO_BIN_EXE_vyrn"))
        .arg("run")
        .arg("--profile")
        .arg(&file)
        .output()
        .expect("vyrn run --profile");
    assert!(profiled.status.success());
    assert_eq!(
        plain.stdout, profiled.stdout,
        "the profile changed the program's own output"
    );
    let table = String::from_utf8_lossy(&profiled.stderr).replace("\r\n", "\n");
    assert!(table.contains("function"), "no table on stderr:\n{table}");
    assert!(table.contains("work"), "`work` is missing:\n{table}");
    assert!(table.contains("main"), "`main` is missing:\n{table}");
    assert!(
        String::from_utf8_lossy(&plain.stderr).trim().is_empty(),
        "an unprofiled run printed a table"
    );

    // After the file, it is the program's argument and not the CLI's.
    let passed = Command::new(env!("CARGO_BIN_EXE_vyrn"))
        .arg("run")
        .arg(&file)
        .arg("--profile")
        .output()
        .expect("vyrn run file --profile");
    assert!(
        String::from_utf8_lossy(&passed.stderr).trim().is_empty(),
        "a trailing --profile was taken by the CLI"
    );
    // `args()` saw it, so the program's own output changed by exactly one.
    assert_ne!(plain.stdout, passed.stdout);
}
