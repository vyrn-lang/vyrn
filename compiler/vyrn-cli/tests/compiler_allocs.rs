//! Pins the compiler's own allocations per phase.
//!
//! `VYRN_BUILD_PROFILE=allocs` makes a `vyrn` built with `--features allocs`
//! print the allocations each build phase made. This test runs four builds on
//! one thread and holds each phase's count to `tests/pins/compiler-allocs.tsv`:
//!
//!   - a count above its pin plus the band fails, naming the phase;
//!   - a count below the pin minus the band passes and asks for a re-pin,
//!     because a count only shrinks;
//!   - a phase with no row, or a row with no phase, fails.
//!
//! The band is 1 percent, at least 2. Five single-thread runs of
//! `site/export.vyrn` differed by at most 10 allocations in a phase of 79,848.
//! Reallocations and bytes are not pinned: an incremental build and a
//! non-incremental one differ by 5 percent in the reallocations of the loader,
//! and `placer` varies 2 percent in bytes from run to run.
//!
//! The pin is Windows only: the standard library's path and file calls allocate
//! differently on each OS. On any other OS the test prints a note and passes.
//!
//!   cargo test --release -p vyrn-cli --features allocs --test compiler_allocs -- --ignored

#![cfg(feature = "allocs")]

mod common;
use common::*;
use std::path::Path;

/// The verb and root of each measured build: the largest program, then the two
/// largest examples under the checker alone.
const ROOTS: &[(&str, &str)] = &[
    ("build", "site/export.vyrn"),
    ("check", "site/export.vyrn"),
    ("check", "examples/simdbench.vyrn"),
    ("check", "examples/vlog.vyrn"),
];

/// Allowed growth over a pin: 1 percent, and never less than 2 allocations.
fn band(pinned: u64) -> u64 {
    (pinned / 100).max(2)
}

fn pin_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/pins/compiler-allocs.tsv")
}

#[test]
#[ignore = "builds site/export.vyrn four times on one thread; named in the gate list"]
fn compiler_allocations_stay_inside_their_pin() {
    if std::env::consts::OS != "windows" {
        eprintln!("compiler_allocs: the pin holds Windows counts; skipped on this OS");
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cache = scratch("allocs-gen");
    let out = scratch("allocs-out");
    let wasm = out.join("out.wasm");
    let mut got: Vec<(String, String, u64)> = Vec::new();
    for &(verb, file) in ROOTS {
        let wasm = wasm.to_str().unwrap();
        let args: Vec<&str> = match verb {
            "build" => vec!["build", "--target", "wasm", file, "-o", wasm],
            _ => vec![verb, file],
        };
        // The first run fills the generator cache; its counts are not the pin's.
        alloc_phases(&args, &root, &cache);
        for (phase, n, _) in alloc_phases(&args, &root, &cache) {
            got.push((format!("{verb} {file}"), phase, n));
        }
    }

    let text = std::fs::read_to_string(pin_path()).unwrap_or_default();
    if pin_write() {
        let mut out = String::from(
            "# Allocations per compiler phase, Windows, one thread, warm generator cache.\n\
             # root, phase, allocations. Written by VYRN_PIN=write; see tests/compiler_allocs.rs.\n",
        );
        for (r, p, n) in &got {
            out.push_str(&format!("{r}\t{p}\t{n}\n"));
        }
        std::fs::write(pin_path(), out).expect("write pin");
        return;
    }

    let pinned: Vec<(&str, &str, u64)> = text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            (c[0], c[1], c[2].parse().expect("pinned count"))
        })
        .collect();
    assert!(
        !pinned.is_empty(),
        "tests/pins/compiler-allocs.tsv has no rows; write them with VYRN_PIN=write"
    );
    let mut failures = Vec::new();
    let mut shrunk = Vec::new();
    for (root, phase, n) in &got {
        match pinned.iter().find(|p| p.0 == root && p.1 == phase) {
            None => failures.push(format!("{root}: phase `{phase}` has no pin; it made {n}")),
            Some(&(_, _, pin)) if *n > pin + band(pin) => failures.push(format!(
                "{root}: phase `{phase}` made {n} allocations, pinned {pin} (limit {})",
                pin + band(pin)
            )),
            Some(&(_, _, pin)) if n + band(pin) < pin => {
                shrunk.push(format!("{root}: phase `{phase}` made {n}, pinned {pin}"))
            }
            Some(_) => {}
        }
    }
    for &(root, phase, pin) in &pinned {
        if !got.iter().any(|g| g.0 == root && g.1 == phase) {
            failures.push(format!(
                "{root}: pinned phase `{phase}` ({pin}) did not run"
            ));
        }
    }
    if !shrunk.is_empty() {
        eprintln!(
            "allocations shrank; re-pin with VYRN_PIN=write so the ratchet holds:\n{}",
            shrunk.join("\n")
        );
    }
    assert!(
        failures.is_empty(),
        "the compiler's allocations moved; a count only shrinks, so find the allocation or re-pin by hand with a reason\n{}",
        failures.join("\n")
    );
}
