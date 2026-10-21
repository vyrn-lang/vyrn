//! Ratchet on wasmtime fuel: the wasm instructions each Benchmarks Game program and the
//! `fromJson` benchmark spend on their committed input.
//!
//! Fuel counts instructions at wasmtime's prices. It does not see native code, memory latency,
//! inlining or the length of a `memory.copy`, so a fuel row never licenses a speed claim; it
//! catches a change that makes the emitter spend more instructions. A count does not move with
//! the machine, so the pin is exact data and the threshold is a budget, not a noise band.
//!
//! `tests/pins/fuel.tsv` holds the count per program. `VYRN_PIN=write` rewrites it. Fuel may grow
//! by `VYRN_FUEL_BUDGET` percent (default 0.1) over its row. `VYRN_FUEL_BASE` names another pin
//! to compare against, so CI measures a pull request against the pin of the commit it builds on.
//! A program that pin has no row for, one the pull request adds, is held to the tree's own pin.
//! Against the tree's own pin a count that shrinks by more than the budget also fails: re-pin it.

mod common;
use common::*;

/// The programs `benchgame.rs` checks and `jsondecode`, with the fixture each prints.
const PROGRAMS: &[(&str, &str)] = &[
    ("nbody", "nbody-1000.expected"),
    ("spectralnorm", "spectralnorm-100.expected"),
    ("fannkuch", "fannkuch-7.expected"),
    ("binarytrees", "binarytrees-10.expected"),
    ("fasta", "fasta-1000.expected"),
    ("revcomp", "revcomp-1000.expected"),
    ("knucleotide", "knucleotide-1000.expected"),
    ("pidigits", "pidigits-27.expected"),
    ("mandelbrot", "mandelbrot-200.expected"),
    ("regexredux", "regexredux-1000.expected"),
    ("jsondecode", "../examples/expected/jsondecode.stdout"),
];

const HEADER: &str = "program\tfuel";

fn pin_file() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/pins/fuel.tsv")
}

/// Runs `name` on its committed input under `VYRN_FUEL` and returns the fuel `_start` spent.
fn fuel_of(name: &str, fixture: &str, log_dir: &std::path::Path) -> u64 {
    let dir = examples_dir();
    let path = dir.join(format!("{name}.vyrn"));
    let log = log_dir.join(format!("{name}.fuel"));
    let mut cmd = vyrn();
    cmd.env("VYRN_FUEL", &log).arg("run").arg(&path);
    let out = run_io(cmd, &dir, &path.with_extension("stdin"));
    let err = runtime_err(&out.stderr);
    assert!(
        err.is_empty() && out.status.code() == Some(0),
        "{name}: exited {:?} with stderr:\n{err}",
        out.status.code()
    );
    let want = norm(&std::fs::read(examples_dir().join("../bench").join(fixture)).expect(fixture));
    if let Some(diff) = first_diff("stdout", "expected", &want, name, &norm(&out.stdout)) {
        panic!("{name}: does not print {fixture}\n{diff}");
    }
    let text = std::fs::read_to_string(&log).unwrap_or_else(|e| panic!("{name}: no fuel log: {e}"));
    text.trim()
        .rsplit('\t')
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("{name}: fuel log is not `<argv0>\t<count>`: {text:?}"))
}

fn read_pin(file: &std::path::Path) -> std::collections::BTreeMap<String, u64> {
    let text = std::fs::read_to_string(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
    let mut rows = text.lines();
    assert_eq!(rows.next(), Some(HEADER), "{}: header", file.display());
    rows.map(|l| {
        let (name, n) = l
            .split_once('\t')
            .unwrap_or_else(|| panic!("bad fuel row {l:?}"));
        (
            name.to_string(),
            n.parse().unwrap_or_else(|_| panic!("bad fuel row {l:?}")),
        )
    })
    .collect()
}

#[test]
#[ignore = "runs every benchmark program; run explicitly: cargo test --release -p vyrn-cli --test fuel -- --ignored"]
fn no_benchmark_program_spends_more_fuel_than_its_pin() {
    let scratch = scratch("fuel");
    let got: Vec<(String, u64)> = PROGRAMS
        .iter()
        .map(|(name, fixture)| (name.to_string(), fuel_of(name, fixture, &scratch)))
        .collect();

    if pin_write() {
        let rows: String = got.iter().map(|(n, f)| format!("{n}\t{f}\n")).collect();
        std::fs::write(pin_file(), format!("{HEADER}\n{rows}")).expect("write the pin");
        return;
    }
    let base = std::env::var_os("VYRN_FUEL_BASE");
    let own = read_pin(&pin_file());
    let pinned = base.as_ref().map(|b| read_pin(std::path::Path::new(b)));
    let budget: f64 = std::env::var("VYRN_FUEL_BUDGET").map_or(0.1, |b| {
        b.parse()
            .unwrap_or_else(|_| panic!("VYRN_FUEL_BUDGET: {b:?}"))
    });

    let mut failures = Vec::new();
    for (name, fuel) in &got {
        let from_base = pinned.as_ref().and_then(|p| p.get(name));
        let Some(&was) = from_base.or_else(|| own.get(name)) else {
            failures.push(format!(
                "{name}: no row in the pin; add it with VYRN_PIN=write"
            ));
            continue;
        };
        let change = (*fuel as f64 - was as f64) / was as f64 * 100.0;
        if change > budget {
            failures.push(format!(
                "{name}: {was} -> {fuel} fuel, +{change:.3}% (budget {budget}%)"
            ));
        } else if from_base.is_none() && change < -budget {
            failures.push(format!(
                "{name}: {was} -> {fuel} fuel, {change:.3}%; re-pin with VYRN_PIN=write"
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
