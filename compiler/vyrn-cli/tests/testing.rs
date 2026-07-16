//! `vyrn test` integration tests (RFC-0015): exact stdout, exit codes, the
//! `--name` filter, the no-tests case, and IR stripping. Interpreter-only (no
//! clang), so these run in the default suite.

use std::path::PathBuf;
use std::process::Command;

fn vyrn() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vyrn"))
}

/// A fresh scratch directory for one test.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("vyrn-testing-tests").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn norm(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace("\r\n", "\n")
}

#[test]
fn runs_passing_and_failing_tests_with_exact_output() {
    let dir = scratch("mixed");
    let file = dir.join("t.vyrn");
    std::fs::write(
        &file,
        "test \"one plus one\" {\n\
         \x20   assertEq(1 + 1, 2)\n\
         }\n\
         test \"prints then fails\" {\n\
         \x20   print(7)\n\
         \x20   assert(1 == 2)\n\
         }\n\
         test \"mismatch message\" {\n\
         \x20   assertEq(3 + 4, 8)\n\
         }\n",
    )
    .unwrap();
    let out = vyrn().arg("test").arg(&file).output().unwrap();
    // Any failing test -> exit 1.
    assert_eq!(out.status.code(), Some(1));
    let stdout = norm(&out.stdout);
    let expected = "test \"one plus one\" ... ok\n\
                    7\n\
                    test \"prints then fails\" ... FAILED: assertion failed at line 6\n\
                    test \"mismatch message\" ... FAILED: assertion failed at line 9: 7 != 8\n\
                    \n\
                    1 passed, 2 failed\n";
    assert_eq!(stdout, expected, "got:\n{stdout}");
}

#[test]
fn all_passing_exits_zero() {
    let dir = scratch("allpass");
    let file = dir.join("t.vyrn");
    std::fs::write(
        &file,
        "test \"a\" { assert(true) }\ntest \"b\" { assertEq(2, 2) }\n",
    )
    .unwrap();
    let out = vyrn().arg("test").arg(&file).output().unwrap();
    assert!(out.status.success());
    let stdout = norm(&out.stdout);
    assert_eq!(
        stdout,
        "test \"a\" ... ok\ntest \"b\" ... ok\n\n2 passed, 0 failed\n",
        "got:\n{stdout}"
    );
}

#[test]
fn name_filter_selects_a_subset() {
    let dir = scratch("filter");
    let file = dir.join("t.vyrn");
    std::fs::write(
        &file,
        "test \"alpha\" { assert(true) }\n\
         test \"beta\" { assert(true) }\n\
         test \"alphabet\" { assert(true) }\n",
    )
    .unwrap();
    let out = vyrn().arg("test").arg(&file).args(["--name", "alpha"]).output().unwrap();
    assert!(out.status.success());
    let stdout = norm(&out.stdout);
    assert_eq!(
        stdout,
        "test \"alpha\" ... ok\ntest \"alphabet\" ... ok\n\n2 passed, 0 failed\n",
        "got:\n{stdout}"
    );
}

#[test]
fn no_tests_prints_no_tests_and_exits_zero() {
    let dir = scratch("none");
    let file = dir.join("t.vyrn");
    std::fs::write(&file, "fn main() -> Int64 { return 0 }\n").unwrap();
    let out = vyrn().arg("test").arg(&file).output().unwrap();
    assert!(out.status.success());
    assert_eq!(norm(&out.stdout), "no tests\n");
}

#[test]
fn a_file_may_have_both_tests_and_a_main() {
    // `run` executes `main` (tests stripped); `test` runs the tests.
    let dir = scratch("both");
    let file = dir.join("t.vyrn");
    std::fs::write(
        &file,
        "test \"t\" { assertEq(6 * 7, 42) }\n\
         fn main() -> Int64 { print(99) return 0 }\n",
    )
    .unwrap();
    let run = vyrn().arg("run").arg(&file).output().unwrap();
    assert!(run.status.success());
    assert_eq!(norm(&run.stdout).trim(), "99");
    let test = vyrn().arg("test").arg(&file).output().unwrap();
    assert!(test.status.success());
    assert_eq!(norm(&test.stdout), "test \"t\" ... ok\n\n1 passed, 0 failed\n");
}

#[test]
fn test_bodies_are_stripped_from_emitted_ir() {
    // A test body's unique string literal must not reach codegen.
    let dir = scratch("strip");
    let file = dir.join("t.vyrn");
    std::fs::write(
        &file,
        "test \"UNIQUE_TEST_MARKER\" { let s = \"SECRET_IN_TEST_BODY\" print(s.length) }\n\
         fn main() -> Int64 { print(1) return 0 }\n",
    )
    .unwrap();
    let out = vyrn().arg("emit-ir").arg(&file).output().unwrap();
    assert!(out.status.success(), "{}", norm(&out.stderr));
    let ir = norm(&out.stdout);
    assert!(!ir.contains("SECRET_IN_TEST_BODY"), "test string leaked into IR");
    assert!(!ir.contains("UNIQUE_TEST_MARKER"), "test name leaked into IR");
}
