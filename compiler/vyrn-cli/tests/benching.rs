//! `vyrn bench` integration tests (RFC-0055). The `--check` face is deterministic
//! (interpreter-only, no clang) and pinned byte-for-byte; the native timing face
//! needs clang, so its smoke test is `#[ignore]`d and asserts SHAPE (regex), never
//! the numbers. Also: `blackBox` placement rules and the bench-stripping guarantee.

use std::path::PathBuf;
use std::process::Command;

fn vyrn() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vyrn"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("vyrn-benching-tests").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn norm(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace("\r\n", "\n")
}

// ---- `--check` (the deterministic, byte-pinnable face) ----------------------

#[test]
fn check_runs_each_body_once_with_exact_output() {
    let dir = scratch("check-mixed");
    let file = dir.join("b.vyrn");
    std::fs::write(
        &file,
        "bench \"ok one\" {\n\
         \x20   blackBox(1 + 1)\n\
         }\n\
         bench \"traps\" {\n\
         \x20   let mut xs: Array<Int64> = []\n\
         \x20   blackBox(xs[0])\n\
         }\n\
         bench \"ok two\" {\n\
         \x20   blackBox(2)\n\
         }\n",
    )
    .unwrap();
    let out = vyrn().arg("bench").arg(&file).arg("--check").output().unwrap();
    // A trapping bench -> exit 1, but the run CONTINUES to the next bench.
    assert_eq!(out.status.code(), Some(1));
    let stdout = norm(&out.stdout);
    let expected = "bench \"ok one\" ... ok\n\
                    bench \"traps\" ... FAILED: array index 0 out of bounds\n\
                    bench \"ok two\" ... ok\n\
                    \n\
                    2 ok, 1 failed\n";
    assert_eq!(stdout, expected, "got:\n{stdout}");
}

#[test]
fn check_all_ok_exits_zero() {
    let dir = scratch("check-ok");
    let file = dir.join("b.vyrn");
    std::fs::write(
        &file,
        "bench \"a\" { blackBox(1) }\nbench \"b\" { blackBox(2) }\n",
    )
    .unwrap();
    let out = vyrn().arg("bench").arg(&file).arg("--check").output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        norm(&out.stdout),
        "bench \"a\" ... ok\nbench \"b\" ... ok\n\n2 ok, 0 failed\n"
    );
}

#[test]
fn check_name_filter_selects_a_subset() {
    let dir = scratch("check-filter");
    let file = dir.join("b.vyrn");
    std::fs::write(
        &file,
        "bench \"alpha\" { blackBox(1) }\n\
         bench \"beta\" { blackBox(2) }\n\
         bench \"alphabet\" { blackBox(3) }\n",
    )
    .unwrap();
    let out = vyrn().arg("bench").arg(&file).args(["--check", "--name", "alpha"]).output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        norm(&out.stdout),
        "bench \"alpha\" ... ok\nbench \"alphabet\" ... ok\n\n2 ok, 0 failed\n"
    );
}

#[test]
fn no_benches_prints_no_benches_and_exits_zero() {
    let dir = scratch("check-none");
    let file = dir.join("b.vyrn");
    std::fs::write(&file, "fn main() -> Int64 { return 0 }\n").unwrap();
    let out = vyrn().arg("bench").arg(&file).arg("--check").output().unwrap();
    assert!(out.status.success());
    assert_eq!(norm(&out.stdout), "no benches\n");
}

// ---- `blackBox` placement (bench/test bodies only) --------------------------

#[test]
fn blackbox_outside_a_bench_or_test_is_a_checker_error() {
    let dir = scratch("bb-outside");
    let file = dir.join("b.vyrn");
    std::fs::write(
        &file,
        "fn main() -> Int64 { let x = blackBox(1) return x }\n",
    )
    .unwrap();
    let out = vyrn().arg("check").arg(&file).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = norm(&out.stderr);
    assert!(err.contains("`blackBox` is only available inside a `bench` or `test` block"), "got:\n{err}");
}

#[test]
fn blackbox_inside_bench_and_test_is_accepted() {
    let dir = scratch("bb-inside");
    let file = dir.join("b.vyrn");
    std::fs::write(
        &file,
        "bench \"b\" { blackBox(1) }\n\
         test \"t\" { assertEq(blackBox(2), 2) }\n\
         fn main() -> Int64 { return 0 }\n",
    )
    .unwrap();
    let out = vyrn().arg("check").arg(&file).output().unwrap();
    assert!(out.status.success(), "stderr:\n{}", norm(&out.stderr));
    assert_eq!(norm(&out.stdout), "ok\n");
}

// ---- strip guarantee --------------------------------------------------------

#[test]
fn bench_bodies_are_stripped_from_emitted_ir() {
    // A bench body's unique string literal must not reach codegen (run/build/
    // emit-ir walk only `functions`, exactly like tests).
    let dir = scratch("strip");
    let file = dir.join("b.vyrn");
    std::fs::write(
        &file,
        "bench \"UNIQUE_BENCH_MARKER\" { let s = \"SECRET_IN_BENCH_BODY\" blackBox(s.byteLength) }\n\
         fn main() -> Int64 { print(1) return 0 }\n",
    )
    .unwrap();
    let out = vyrn().arg("emit-ir").arg(&file).output().unwrap();
    assert!(out.status.success(), "{}", norm(&out.stderr));
    let ir = norm(&out.stdout);
    assert!(!ir.contains("SECRET_IN_BENCH_BODY"), "bench string leaked into IR");
    assert!(!ir.contains("UNIQUE_BENCH_MARKER"), "bench name leaked into IR");
    // And no optimizer barrier leaks into an ordinary compile.
    assert!(!ir.contains("asm sideeffect"), "blackBox barrier leaked into a non-bench compile");
}

#[test]
fn a_file_may_have_both_benches_and_a_main() {
    // `run` executes `main` (benches stripped); `bench --check` runs the benches.
    let dir = scratch("both");
    let file = dir.join("b.vyrn");
    std::fs::write(
        &file,
        "bench \"b\" { blackBox(6 * 7) }\n\
         fn main() -> Int64 { print(99) return 0 }\n",
    )
    .unwrap();
    let run = vyrn().arg("run").arg(&file).output().unwrap();
    assert!(run.status.success());
    assert_eq!(norm(&run.stdout).trim(), "99");
    let bench = vyrn().arg("bench").arg(&file).arg("--check").output().unwrap();
    assert!(bench.status.success());
    assert_eq!(norm(&bench.stdout), "bench \"b\" ... ok\n\n1 ok, 0 failed\n");
}

// ---- native timing smoke (needs clang) --------------------------------------

#[test]
#[ignore = "needs clang; run explicitly: cargo test -p vyrn-cli --test benching -- --ignored"]
fn native_bench_reports_the_expected_shape() {
    // A real `vyrn bench` compile+run. We assert only the report SHAPE — names,
    // unit suffixes, sample/iter counts — never the timing numbers (which vary).
    let dir = scratch("native");
    let file = dir.join("b.vyrn");
    std::fs::write(
        &file,
        "fn hashTo(n: Int64) -> Int64 {\n\
         \x20   let mut h = 0\n\
         \x20   let mut i = 0\n\
         \x20   while i < n {\n\
         \x20       h = (h * 31 + i) % 1000000007\n\
         \x20       i = i + 1\n\
         \x20   }\n\
         \x20   return h\n\
         }\n\
         bench \"hash\" { blackBox(hashTo(blackBox(200))) }\n\
         bench \"push\" { let mut xs: Array<Int64> = [] let mut i = 0 while i < 200 { xs.push(i) i = i + 1 } blackBox(xs.length) }\n\
         fn main() -> Int64 { return 0 }\n",
    )
    .unwrap();
    let out = vyrn().arg("bench").arg(&file).output().unwrap();
    assert!(out.status.success(), "stderr:\n{}", norm(&out.stderr));
    let stdout = norm(&out.stdout);
    // Shape: `bench "name"   min <num> <unit>   median <num> <unit>   mean <num> <unit>   (N samples × M iters)`.
    let line = regex_like(&stdout, "bench \"hash\"");
    assert!(line.is_some(), "missing hash line:\n{stdout}");
    for name in ["hash", "push"] {
        let l = regex_like(&stdout, &format!("bench \"{name}\"")).unwrap();
        assert!(l.contains(" min "), "no min column: {l}");
        assert!(l.contains(" median "), "no median column: {l}");
        assert!(l.contains(" mean "), "no mean column: {l}");
        assert!(
            l.contains(" ns") || l.contains(" µs") || l.contains(" ms") || l.contains(" s "),
            "no time unit suffix: {l}"
        );
        assert!(l.contains(" samples × ") && l.contains(" iters)"), "no sample/iter counts: {l}");
    }
    assert!(stdout.contains("\n2 benches\n"), "missing footer:\n{stdout}");
}

/// The first line of `text` that starts with `needle` (a tiny shape helper so the
/// smoke test needs no regex crate).
fn regex_like<'a>(text: &'a str, needle: &str) -> Option<&'a str> {
    text.lines().find(|l| l.starts_with(needle))
}
