//! Every `test` block in `std/` runs, and the count is read off the source.
//!
//! WHAT THIS REPLACES. std coverage used to be an opt-in list: fifteen
//! hand-written wrappers in a dozen topic test files, each naming one module and
//! (mostly) one hand-typed expected count. Twenty-three std modules carry `test`
//! blocks; eight of them — `std/i18n` (16 blocks), `std/args` (8),
//! `std/jsondec` (7), `std/bench` (5), `std/diag` (4), `std/math` (3),
//! `std/openapi` (3), `std/connect` (2) — were on nobody's list, so 48 assertions
//! ran in no gate at all. A list of what to check is a list somebody has to
//! remember to add to; a sweep is not.
//!
//! WHY THE COUNT IS DERIVED AND NOT WRITTEN DOWN. `vyrn test` on a file with no
//! `test` blocks prints "no tests" and exits 0 (RFC-0015: a file without tests is
//! not an error). So "the suite passed" and "the suite is gone" are the same exit
//! code, and a wrapper that stops matching its target degrades to success. Two
//! ways out of that:
//!
//!   1. make `vyrn test` exit non-zero on an empty run, or
//!   2. make the sweep assert a count it did not get from the runner.
//!
//! This takes (2), for two reasons. It is strictly stronger: a hand-written
//! floor only catches a suite dropping to zero, while the count scanned out of
//! the source catches `16 -> 15` as well — a `test` block deleted, or renamed
//! into something the runner no longer discovers, fails here with both numbers
//! in the message. And it leaves the CLI's contract alone: `vyrn test` on a
//! module with no tests is a legitimate thing for a user to do, and `site.yml`
//! and `docs/` both rely on it.
//!
//! The fifteen hand-written wrappers this supersedes are still in the topic test
//! files (`std_codecs_unit_tests_run_green`, `std_ui_unit_tests_run_green`, …).
//! They are redundant now, not wrong: each re-runs one module this sweep already
//! runs, against a hand-typed count. Deleting them is a separate change with no
//! gate consequence.
//!
//! The scan is deliberately the crudest thing that can work — `^test "` at
//! column zero, which is what `vyrn fmt` produces for every test block in the
//! tree. It is a FLOOR, not a parse: it never over-counts (a nested or indented
//! `test "` is missed, making the assertion weaker, never wrong), and `vyrn fmt
//! --check` in the same CI job is what keeps the spelling canonical.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The repository root — two levels up from `compiler/vyrn-cli`.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

/// A path in the loader-parseable spelling (no `\\?\`, forward slashes).
fn loader_path(p: &Path) -> PathBuf {
    let s = p.to_string_lossy().replace('\\', "/");
    PathBuf::from(s.strip_prefix("//?/").unwrap_or(&s).to_string())
}

/// `test "` blocks at column zero — the floor this sweep holds the runner to.
fn declared_blocks(src: &str) -> usize {
    src.lines().filter(|l| l.starts_with("test \"")).count()
}

/// The number `vyrn test` reported as passing, from its `N passed, M failed`
/// summary line.
fn reported_passed(output: &str) -> Option<usize> {
    output
        .lines()
        .rev()
        .find_map(|l| l.trim().strip_suffix(", 0 failed"))
        .and_then(|l| l.strip_suffix(" passed"))
        .and_then(|n| n.trim().parse().ok())
}

/// Every `.vyrn` file directly under `std/`, sorted so a failure names the same
/// module on every machine.
fn std_modules() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(repo_root().join("std"))
        .expect("read std/")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "vyrn"))
        .collect();
    out.sort();
    assert!(out.len() > 20, "std/ has only {} modules?", out.len());
    out
}

/// The gate. Every std module's own `test` blocks, discovered rather than
/// enumerated, all of them green, and none of them quietly missing.
#[test]
fn every_test_block_in_std_runs_and_passes() {
    let mut ran = 0usize;
    let mut modules = 0usize;
    let mut failures = Vec::new();

    for path in std_modules() {
        let src = std::fs::read_to_string(&path).expect("read a std module");
        let declared = declared_blocks(&src);
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if declared == 0 {
            continue;
        }
        modules += 1;

        let out = Command::new(env!("CARGO_BIN_EXE_vyrn"))
            .arg("test")
            .arg(loader_path(&path))
            .output()
            .expect("spawn vyrn test");
        let combined = String::from_utf8_lossy(&out.stdout).to_string()
            + &String::from_utf8_lossy(&out.stderr);

        if !out.status.success() {
            failures.push(format!("std/{name}: `vyrn test` failed:\n{combined}"));
            continue;
        }
        match reported_passed(&combined) {
            // The empty-run case the census names: a green exit that ran
            // nothing, or ran fewer blocks than the file declares.
            None => failures.push(format!(
                "std/{name}: {declared} `test` blocks in the source, but the run \
                 reported no green summary — a suite that stopped being \
                 discovered:\n{combined}"
            )),
            Some(passed) if passed < declared => failures.push(format!(
                "std/{name}: {declared} `test` blocks in the source, {passed} ran"
            )),
            Some(passed) => ran += passed,
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    // A sweep that discovered nothing is the failure mode this test exists to
    // remove, so it cannot be its own blind spot.
    assert!(
        modules >= 20 && ran >= 200,
        "the sweep found only {ran} tests across {modules} modules — discovery broke"
    );
    eprintln!("std sweep: {ran} test blocks across {modules} modules");
}
