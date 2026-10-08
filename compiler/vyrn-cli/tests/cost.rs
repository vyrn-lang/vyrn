//! `vyrn why --cost` over four programs, each pinned in `tests/cost/`.
//!
//! The pin is the command's whole standard output, run from the repository
//! root so the header names a relative path. `VYRN_PIN=write` rewrites it.
//! Each test saves profiles in a directory of its own (`VYRN_PROFILE_DIR`), so the
//! pins do not depend on the profiles the machine's user has saved.

mod common;
use common::*;
use std::path::Path;

fn root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// An empty directory for the profiles of the test `name`.
fn profiles(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("vyrn-cost-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// The standard output of `vyrn why --cost file`, from the repository root.
fn cost(file: &str, profiles: &Path) -> String {
    let out = vyrn()
        .current_dir(root())
        .env("VYRN_PROFILE_DIR", profiles)
        .args(["why", "--cost", file])
        .output()
        .expect("vyrn why --cost");
    assert!(out.status.success(), "{}", norm(&out.stderr));
    norm(&out.stdout)
}

/// Holds the output of `vyrn why --cost file` equal to `tests/cost/<name>.txt`, with no profile
/// saved.
fn holds(name: &str, file: &str) {
    holds_with(name, file, &profiles(name));
}

fn holds_with(name: &str, file: &str, profiles: &Path) {
    let got = cost(file, profiles);
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

/// `vyrn run --profile` saves its counts, and `why --cost` prints them beside the rows whose
/// source and imports it ran.
#[test]
fn the_last_run_shows_beside_the_rows() {
    let dir = profiles("last-run");
    let run = vyrn()
        .current_dir(root())
        .env("VYRN_PROFILE_DIR", &dir)
        .args(["run", "--profile", "examples/knucleotide.vyrn"])
        .stdin(std::fs::File::open(root().join("examples/knucleotide.stdin")).unwrap())
        .output()
        .expect("vyrn run --profile");
    assert!(run.status.success(), "{}", norm(&run.stderr));
    holds_with("knucleotide-last-run", "examples/knucleotide.vyrn", &dir);
}

/// `word` renders a number into a String and concatenates: 3 blocks, 40 bytes per call.
const WORD: &str = "fn word(n: Int64) -> String {
    return \"w\\{n}\"
}

fn main() -> Int64 {
    print(word(7))
    return 0
}
";

/// An edit to the source after the run makes the profile stale: one line says so, and no row
/// carries a count.
#[test]
fn an_edited_source_makes_the_last_run_stale() {
    let dir = profiles("stale");
    let src = std::env::temp_dir().join("vyrn-cost-stale.vyrn");
    let text = WORD;
    std::fs::write(&src, text).unwrap();
    let file = src.to_string_lossy().replace('\\', "/");
    let run = vyrn()
        .env("VYRN_PROFILE_DIR", &dir)
        .args(["run", "--profile", &file])
        .output()
        .expect("vyrn run --profile");
    assert!(run.status.success(), "{}", norm(&run.stderr));
    let fresh = cost(&file, &dir);
    assert!(fresh.contains("last run: 3 blocks, 40 bytes"), "{fresh}");
    std::fs::write(&src, format!("{text}\n")).unwrap();
    let stale = cost(&file, &dir);
    assert!(stale.contains("profile: stale"), "{stale}");
    assert!(!stale.contains("last run"), "{stale}");
}
