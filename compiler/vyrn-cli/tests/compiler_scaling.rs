//! Fails when a compiler phase allocates faster than its input grows.
//!
//! Each shape is a program generated at `N` and at `4N` items and built to
//! wasm on one thread under `VYRN_BUILD_PROFILE=allocs`. A phase that is
//! linear in the program makes 4 times the allocations and requests 4 times the
//! bytes at `4N`. A fixed cost lowers the ratio and nothing linear raises it.
//! The limit is 4.6, growth of exponent 1.10, so 15 percent over linear. The
//! counts of a linear phase repeat to 1 percent, so the margin is for the
//! growth that is not linear. An `n log n` phase is at 4.8 from 1000 to 4000
//! items and fails; a quadratic one is at 16.
//!
//! Bytes matter as much as counts. A pass that rebuilds a table of the whole
//! body for each of its `n` names makes `log n` allocations per rebuild but
//! requests `n` bytes: the revert of 80cae5be9 (`core_alias` and its
//! siblings) leaves the count of `codegen: bodies` at 1.6 times for the shape
//! `many locals`, and the bytes at 13.8 times.
//!
//! This test sees what `compiler_allocs.rs` cannot: the pin is a snapshot of
//! three programs, and a pass that is quadratic in a shape none of them has
//! stays inside it.
//!
//!   cargo test --release -p vyrn-cli --features allocs --test compiler_scaling -- --ignored

#![cfg(feature = "allocs")]

mod common;
use common::*;
use std::fmt::Write;

const SMALL: usize = 1000;
const LIMIT: f64 = 4.6;
/// Added to the limit, so a phase of a few dozen allocations or a few KiB is
/// not a ratio: one extra table is no growth.
const TINY_COUNT: f64 = 64.0;
const TINY_BYTES: f64 = 65536.0;

/// Phases that are superlinear today, as `(shape, phase)`, each with its
/// cause. The test fails when one stops being superlinear, so a fix removes
/// its row.
const KNOWN: &[(&str, &str, &str)] = &[
    (
        "many generic instances",
        "placer: effects",
        "n instances of one generic function: bytes 18,861,813 to 271,300,981 (14.4 times), allocations 3.5 times",
    ),
];

/// The programs of one size. Each reaches every item from `main`.
fn shapes(n: usize) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();

    // Many functions: a binary tree of calls, so every function is reachable
    // and the call chain is `log n` deep.
    let mut s = String::new();
    for i in 0..n {
        let kids: String = [2 * i + 1, 2 * i + 2]
            .iter()
            .filter(|&&k| k < n)
            .map(|k| format!(" + f{k}(x)"))
            .collect();
        writeln!(s, "fn f{i}(x: Int64) -> Int64 {{ return x + {i}{kids} }}").unwrap();
    }
    s.push_str("fn main() -> Int64 { return f0(1) }\n");
    out.push(("many functions", s));

    // One long body: `n` statements in `main`.
    let mut s = String::from("fn main() -> Int64 {\n    let mut s: Int64 = 0\n");
    for i in 0..n {
        writeln!(s, "    s = s + {i}").unwrap();
    }
    s.push_str("    return s\n}\n");
    out.push(("one long body", s));

    // Many locals, each read once in one body.
    let mut s = String::from("fn main() -> Int64 {\n    let mut s: Int64 = 0\n");
    for i in 0..n {
        writeln!(s, "    let a{i}: Int64 = {i} * 3").unwrap();
    }
    for i in 0..n {
        writeln!(s, "    s = s + a{i}").unwrap();
    }
    s.push_str("    return s\n}\n");
    out.push(("many locals", s));

    // A long `match`: one enum of `n` variants, one arm each.
    let mut s = String::from("type T =\n");
    for i in 0..n {
        writeln!(s, "    | V{i}").unwrap();
    }
    s.push_str("fn pick(t: T) -> Int64 {\n    return match t {\n");
    for i in 0..n {
        writeln!(s, "        V{i} => {i},").unwrap();
    }
    s.push_str("    }\n}\nfn main() -> Int64 { return pick(V0) }\n");
    out.push(("long match", s));

    // Many generic instances: one generic function at `n` record types.
    let mut s = String::from("fn id<T>(x: T) -> T {\n    return x.copy()\n}\n");
    for i in 0..n {
        writeln!(s, "type R{i} = {{ v: Int64 }}").unwrap();
    }
    s.push_str("fn main() -> Int64 {\n    let mut s: Int64 = 0\n");
    for i in 0..n {
        writeln!(s, "    s = s + id(R{i} {{ v: {i} }}).v").unwrap();
    }
    s.push_str("    return s\n}\n");
    out.push(("many generic instances", s));
    out
}

#[test]
#[ignore = "builds ten generated programs on one thread; named in the gate list"]
fn no_phase_allocates_faster_than_its_input() {
    let dir = scratch("scaling");
    let cache = scratch("scaling-gen");
    let build = |n: usize| -> Vec<(&'static str, Vec<(String, u64, u64)>)> {
        shapes(n)
            .into_iter()
            .map(|(name, src)| {
                let file = dir.join(format!("{}-{n}.vyrn", name.replace(' ', "-")));
                std::fs::write(&file, src).unwrap();
                let wasm = dir.join("out.wasm");
                let args = [
                    "build",
                    "--target",
                    "wasm",
                    file.to_str().unwrap(),
                    "-o",
                    wasm.to_str().unwrap(),
                ];
                (name, alloc_phases(&args, &dir, &cache))
            })
            .collect()
    };
    let (small, large) = (build(SMALL), build(4 * SMALL));
    let mut failures = Vec::new();
    for ((name, a), (_, b)) in small.iter().zip(&large) {
        for (phase, allocs, bytes) in b {
            let (a_allocs, a_bytes) = a
                .iter()
                .find(|p| p.0 == *phase)
                .map_or((0, 0), |p| (p.1, p.2));
            let over = |big: u64, little: u64, tiny: f64| big as f64 > LIMIT * little as f64 + tiny;
            let grew = over(*allocs, a_allocs, TINY_COUNT) || over(*bytes, a_bytes, TINY_BYTES);
            let known = KNOWN.iter().any(|k| (k.0, k.1) == (*name, phase.as_str()));
            let report = format!(
                "{name}: phase `{phase}`: allocations {a_allocs} to {allocs}, bytes {a_bytes} to {bytes}, from {SMALL} to {} items (limit {LIMIT} times)",
                4 * SMALL
            );
            match (grew, known) {
                (true, false) => failures.push(report),
                (false, true) => failures.push(format!(
                    "{report}; it is no longer superlinear, so remove its row from KNOWN"
                )),
                _ => {}
            }
        }
    }
    assert!(
        failures.is_empty(),
        "a phase grew faster than its input
{}",
        failures.join(
            "
"
        )
    );
}
