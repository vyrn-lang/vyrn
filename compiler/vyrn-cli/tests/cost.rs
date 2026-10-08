//! `vyrn why --cost` over four programs, each pinned in `tests/cost/`.
//!
//! The pin is the command's whole standard output, run from the repository
//! root so the header names a relative path. `VYRN_PIN=write` rewrites it.

mod common;
use common::*;
use std::path::Path;

/// Holds the output of `vyrn why --cost file` equal to `tests/cost/<name>.txt`.
fn holds(name: &str, file: &str) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = vyrn()
        .current_dir(&root)
        .args(["why", "--cost", file])
        .output()
        .expect("vyrn why --cost");
    assert!(out.status.success(), "{}", norm(&out.stderr));
    let got = norm(&out.stdout);
    let pin = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/cost/{name}.txt"));
    if pin_write() {
        std::fs::write(&pin, &got).expect("write the pin");
        return;
    }
    let want = std::fs::read_to_string(&pin).unwrap_or_default();
    if let Some(diff) = first_diff(name, "pinned", &want, "reported", &got) {
        panic!("tests/cost/{name}.txt has moved; rewrite it with VYRN_PIN=write\n{diff}");
    }
}

/// Nothing in `advance` or `energy` allocates, copies or keeps a check.
#[test]
fn nbody_costs_nothing_in_its_hot_loops() {
    holds("nbody", "examples/nbody.vyrn");
}

/// A call into `std/strings` counts at the calling line.
#[test]
fn knucleotide_counts_a_std_call_at_its_line() {
    holds("knucleotide", "examples/knucleotide.vyrn");
}

#[test]
fn binarytrees_allocates_in_its_constructor_call() {
    holds("binarytrees", "examples/binarytrees.vyrn");
}

/// A `mut` name rebound to a borrow stores a copy, and the report shows it.
#[test]
fn a_name_rebound_to_a_borrow_copies() {
    holds(
        "rebound",
        "compiler/vyrn-cli/tests/shapes/a-name-rebound-to-a-borrow-keeps-its-copy.vyrn",
    );
}
