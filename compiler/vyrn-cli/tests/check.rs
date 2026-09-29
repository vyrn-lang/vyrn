//! Every `tests/check/<name>.vyrn` is a program, and `<name>.stderr` beside it
//! holds what `vyrn check <name>.vyrn` prints, run from `tests/check/`. A
//! program with no `.stderr`, or an empty one, is accepted: exit 0 and nothing
//! printed. Any other program is refused: a nonzero exit and exactly the bytes
//! of its `.stderr`.
//!
//! Add a test by adding the two files. `VYRN_PIN=write` writes every `.stderr`
//! from the run and deletes the ones an accepted program leaves empty.
//! `VYRN_ONLY=<substring>` runs the programs whose name contains it.

mod common;
use common::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

fn check_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/check")
}

/// Checks one program; returns why it failed, or `None`.
fn verdict(name: &str, write: bool) -> Option<String> {
    let out = vyrn()
        .arg("check")
        .arg(name)
        .current_dir(check_dir())
        .output()
        .expect("run vyrn check");
    let got = norm(&out.stderr);
    let file = check_dir().join(name).with_extension("stderr");
    if write {
        if got.is_empty() {
            let _ = std::fs::remove_file(&file);
        } else {
            std::fs::write(&file, &got).expect("write the .stderr");
        }
    }
    let want = std::fs::read(&file).map(|b| norm(&b)).unwrap_or_default();
    let code = out.status.code();
    if got == want && out.status.success() == want.is_empty() {
        return None;
    }
    Some(format!(
        "{name}: exit {code:?}, and the program is {}\n{}",
        if want.is_empty() {
            "accepted"
        } else {
            "refused"
        },
        first_diff("stderr", "recorded", &want, "run", &got).unwrap_or_default()
    ))
}

#[test]
fn every_program_prints_its_stderr() {
    let only = std::env::var("VYRN_ONLY").unwrap_or_default();
    let mut names: Vec<String> = std::fs::read_dir(check_dir())
        .expect("read tests/check")
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .filter(|n| n.strip_suffix(".vyrn").is_some_and(|s| s.contains(&only)))
        .collect();
    names.sort();
    assert!(
        !names.is_empty(),
        "no program in tests/check matches `{only}`"
    );

    let write = pin_write();
    let next = AtomicUsize::new(0);
    let failures = Mutex::new(Vec::new());
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                while let Some(name) = names.get(next.fetch_add(1, Ordering::Relaxed)) {
                    if let Some(why) = verdict(name, write) {
                        failures.lock().unwrap().push(why);
                    }
                }
            });
        }
    });
    let mut failures = failures.into_inner().unwrap();
    failures.sort();
    assert!(
        failures.is_empty(),
        "{} of {} programs moved; rewrite with VYRN_PIN=write and read the diff\n{}",
        failures.len(),
        names.len(),
        failures.join("\n")
    );
}
