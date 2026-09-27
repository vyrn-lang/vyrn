//! Every `test` block in `std/` runs, and the count is read off the source.
//!
//! `vyrn test` on a file with no `test` blocks exits 0, so a suite
//! that stops being discovered passes. The count scanned from the source
//! catches `16 -> 15` too. The scan is `^test "` at column zero, which `vyrn fmt`
//! produces: a floor that never over-counts, kept canonical by `vyrn fmt
//! --check` in the same CI job.

use std::path::{Path, PathBuf};
use std::process::Command;

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

/// `test "` blocks at column zero: the floor the runner is held to.
fn declared_blocks(src: &str) -> usize {
    src.lines().filter(|l| l.starts_with("test \"")).count()
}

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

/// Every std module's `test` blocks, discovered rather than enumerated, green
/// and none missing.
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
            // A green exit that ran nothing, or fewer blocks than declared.
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
    // A sweep that discovered nothing is the failure this test exists to catch.
    assert!(
        modules >= 20 && ran >= 200,
        "the sweep found only {ran} tests across {modules} modules — discovery broke"
    );
    eprintln!("std sweep: {ran} test blocks across {modules} modules");
}
