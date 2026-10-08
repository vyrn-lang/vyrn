//! The `vyrn` command's own flags: `--version`, `-V` and `--profile`.

use std::process::Command;

fn vyrn() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vyrn"))
}

/// A package manager reads exit 0, one line on stdout, and the crate's version from
/// `CARGO_PKG_VERSION`, not a second copy of the number.
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

/// A piped stdout carries the same bytes with the flag as without it. The flag is the
/// CLI's only before the file; after it, `app.vyrn` gets it as an ordinary argument.
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
    assert!(
        table.contains(" operations"),
        "no count on stderr:
{table}"
    );
    assert!(
        String::from_utf8_lossy(&plain.stderr).trim().is_empty(),
        "an unprofiled run printed a table"
    );

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
    // `args()` saw it, so the program's output changed.
    assert_ne!(plain.stdout, passed.stdout);
}

/// The count is wasmtime's fuel, the one column that does not move with the machine.
/// The build phases and `lines read` print only under `VYRN_BUILD_PROFILE`.
#[test]
fn the_compiled_profile_reports_a_repeatable_count_and_no_phases() {
    let dir = std::env::temp_dir().join("vyrn-cli-profile-wasm");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("p.vyrn");
    std::fs::write(
        &file,
        "fn work(n: Int64) -> Int64 {
             let mut h = 0
             let mut i = 0
             while i < n { h = h + i  i = i + 1 }
             return h
         }
         fn main() -> Int64 {
             print(work(1000))
             return 0
         }
",
    )
    .unwrap();
    let one = || {
        Command::new(env!("CARGO_BIN_EXE_vyrn"))
            .arg("run")
            .arg("--profile")
            .arg(&file)
            .output()
            .expect("vyrn run --profile")
    };
    let plain = Command::new(env!("CARGO_BIN_EXE_vyrn"))
        .arg("run")
        .arg(&file)
        .output()
        .expect("vyrn run");
    let first = one();
    assert!(first.status.success());
    assert_eq!(
        plain.stdout, first.stdout,
        "the profile changed the program's own output"
    );
    assert!(
        String::from_utf8_lossy(&plain.stderr).trim().is_empty(),
        "an unprofiled compiled run printed a table"
    );
    let table = String::from_utf8_lossy(&first.stderr).replace(
        "
", "
",
    );
    for row in ["phase", "lines read"] {
        assert!(
            !table.contains(row),
            "`{row}` is a build profile row:
{table}"
        );
    }
    let build = Command::new(env!("CARGO_BIN_EXE_vyrn"))
        .env("VYRN_BUILD_PROFILE", "1")
        .arg("run")
        .arg("--profile")
        .arg(&file)
        .output()
        .expect("VYRN_BUILD_PROFILE=1 vyrn run --profile");
    let build = String::from_utf8_lossy(&build.stderr);
    for row in [
        "phase",
        "load",
        "compile",
        "translate",
        "instantiate",
        "run",
    ] {
        assert!(
            build.contains(row),
            "`{row}` is missing under VYRN_BUILD_PROFILE:
{build}"
        );
    }
    let count = |text: &str| -> String {
        text.lines()
            .find(|l| l.starts_with("run: "))
            .unwrap_or_else(|| {
                panic!(
                    "no count:
{text}"
                )
            })
            .to_string()
    };
    let second = one();
    assert_eq!(
        count(&table),
        count(&String::from_utf8_lossy(&second.stderr).replace(
            "
", "
"
        )),
        "fuel is a count, so two runs of one program must report the same one"
    );
}

/// The profile counts every block at the line of the row that made it, and a call into `std`
/// counts at the calling line. The totals are the fixture's own: the instrument is the audit
/// plus a counter, so each block made is freed and none is live at exit.
#[test]
fn the_profile_counts_blocks_per_line() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = vyrn()
        .current_dir(&root)
        .args(["run", "--profile", "examples/knucleotide.vyrn"])
        .stdin(std::fs::File::open(root.join("examples/knucleotide.stdin")).unwrap())
        .output()
        .expect("vyrn run --profile");
    assert!(out.status.success());
    let table = String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n");
    assert!(
        table.contains(
            "1,334 blocks, 1,395,504 bytes; 1,334 freed; peak live 335,896 bytes; live at exit 0"
        ),
        "{table}"
    );
    for row in [
        "  96  countKmers  ",
        "grows tally(..)",
        "enters toUpper(..) in std/strings",
    ] {
        assert!(table.contains(row), "no `{row}`:\n{table}");
    }
}
