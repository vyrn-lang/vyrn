//! The exit-residue RATCHET (RFC-0114 §25; restored on the wasm route by
//! RFC-0125 §3 M4).
//!
//! Every corpus program is compiled with `VYRN_LEAK_CHECK=1` in the
//! compiler's environment, which selects `std/runtime`'s accounting allocator
//! (`auditBirth`, `auditDeath`, `auditInit`, `auditExit`) and the module-state
//! teardown behind it. The program then says, at its own exit, whether every
//! block the allocator handed out came back:
//!
//!   - a DOUBLE FREE — a free of a block that is not live — is one line on fd
//!     2 and exit 134, at the site, before the free list is corrupted;
//!   - residue is `free audit: N block(s), M bytes, never freed` and exit 135;
//!   - everything else is the program's own exit code, unchanged.
//!
//! The verdict is compared against the committed baseline
//! (`rfcs/census/residue-baseline.tsv`). The rules, which are the ones the
//! ratchet has always had:
//!
//!   - a DOUBLE FREE fails, whatever the baseline says;
//!   - a `clean` row that now leaks fails — a regression;
//!   - a `leak N` row that leaks MORE than N blocks fails — the ratchet only
//!     turns one way;
//!   - an example with no row must come out clean — new examples do not get
//!     to leak quietly;
//!   - a `leak` row that comes out clean, or smaller, passes and says so —
//!     that is a nudge to shrink the baseline, not an error;
//!   - a `clean` row that stops exiting 0 fails. Parity used to hold that,
//!     and parity is gone: a double free through a pointer wrong enough that
//!     `free` reads its header out of bounds traps before `auditDeath` can
//!     say anything, so the exit code is the only witness left.
//!
//! `other` rows exit nonzero by design (their own exit codes are their
//! outputs); they pass as long as the audit stays quiet.
//!
//! **Two engines, one module.** The instrument is inside the module, so both
//! engines report the same residue for the same program: the leg that runs
//! `vyrn run` needs nothing but the compiler, and the leg that runs the wasm2c
//! route's executable needs clang, wabt and simde and SKIPS without them, the
//! way `route.rs` does. The corpus is `route.rs`'s corpus, minus the same
//! three lists — a program that does not build is not a residue verdict.
//!
//!     cargo test -p vyrn-cli --release --test residue -- --ignored --nocapture

mod common;
use common::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, PartialEq, Clone)]
enum Expect {
    Clean,
    Leak(u64),
    Other,
}

fn baseline() -> HashMap<String, Expect> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rfcs/census/residue-baseline.tsv");
    let text = std::fs::read_to_string(&path).expect("residue baseline");
    let mut out = HashMap::new();
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let mut parts = line.split('\t');
        let name = parts.next().expect("name").to_string();
        let what = parts.next().expect("verdict");
        let e = match what {
            "clean" => Expect::Clean,
            "other" => Expect::Other,
            "leak" => Expect::Leak(
                parts
                    .next()
                    .and_then(|n| n.parse().ok())
                    .expect("leak block count"),
            ),
            other => panic!("unknown baseline verdict `{other}` for `{name}`"),
        };
        out.insert(name, e);
    }
    out
}

/// `free audit: N block(s), ...` — the audit's one stderr line.
fn blocks(stderr: &str) -> Option<u64> {
    let at = stderr.rfind("free audit: ")?;
    let rest = &stderr[at + "free audit: ".len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// The corpus, in `route.rs`'s selection: every example but the ones that do
/// not build and the one only a browser can run.
fn corpus() -> Vec<PathBuf> {
    let dir = examples_dir();
    let mut names: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "vyrn"))
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            !KNOWN_DIVERGENT.iter().any(|(n, _)| *n == name)
                && !EXPECTED_CHECK_FAILURE.iter().any(|(n, ..)| *n == name)
                && !WASM_ONLY.iter().any(|(n, _)| *n == name)
        })
        .collect();
    names.sort();
    assert!(!names.is_empty(), "no examples found in {}", dir.display());
    names
}

/// One program's verdict, from an exit code and a stderr.
fn judge(
    engine: &str,
    name: &str,
    expect: &Expect,
    code: Option<i32>,
    err: &str,
    failures: &mut Vec<String>,
    nudges: &mut usize,
) {
    match code {
        Some(134) => failures.push(format!("{name} ({engine}): DOUBLE FREE\n{err}")),
        Some(135) => {
            let got = blocks(err).unwrap_or(u64::MAX);
            match expect {
                Expect::Leak(max) if got <= *max => {
                    if got < *max {
                        *nudges += 1;
                        eprintln!(
                            "ratchet: {name} ({engine}) leaks {got} (baseline {max}) — shrink its row"
                        );
                    }
                }
                Expect::Leak(max) => failures.push(format!(
                    "{name} ({engine}): residue grew — {got} block(s), baseline allows {max}"
                )),
                _ => failures.push(format!(
                    "{name} ({engine}): new residue — {got} block(s), baseline says {expect:?}"
                )),
            }
        }
        Some(0) => {}
        Some(code) => {
            // The program's own exit code. `other` rows exit nonzero by
            // design; a `leak` row that reaches here came out clean.
            //
            // A `clean` row that stops exiting 0 is a FAILURE, and this rule is
            // new. The old ratchet let any exit code through because parity ran
            // beside it and caught a trap; parity is gone, and the defect this
            // rule catches is the one class the audit cannot report — a double
            // free through a pointer so wrong that `free` reads its header out
            // of bounds and traps before `auditDeath` sees it. Reverting the
            // release-row floor (`403131c9`) is exactly that, on
            // `examples/looptemp.vyrn`, and nothing else in the corpus moves.
            match expect {
                Expect::Leak(_) => {
                    *nudges += 1;
                    eprintln!(
                        "ratchet: {name} ({engine}) is CLEAN now — move its row to `clean`, or \
                         to `other` if it exits {code} by design"
                    );
                }
                Expect::Clean => failures.push(format!(
                    "{name} ({engine}): exited {code}, and its row says clean — a clean row \
                     exits 0, so this is a trap or a refusal the row does not know about\n{err}"
                )),
                Expect::Other => {}
            }
        }
        None => failures.push(format!("{name} ({engine}): killed by signal")),
    }
}

#[test]
#[ignore = "the whole corpus, compiled twice; run explicitly: cargo test -p vyrn-cli --release --test residue -- --ignored"]
fn the_residue_ratchet_only_turns_one_way() {
    let base = baseline();
    let dir = examples_dir();
    let out_dir = scratch("residue");
    // The route's leg is the wasm2c executable; without the tools the engine
    // leg still measures the same module, so a missing tool is a skip of the
    // leg and not of the ratchet.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let route = vyrn_codegen::toolchain::wasm2c_from(&root)
        .ok()
        .flatten()
        .is_some()
        && vyrn_codegen::toolchain::simde_from(&root).is_some()
        && vyrn_codegen::toolchain::find_clang().is_some();
    if !route {
        eprintln!("SKIP the route's leg: clang, wabt or simde is missing");
    }

    let mut failures: Vec<String> = Vec::new();
    let mut nudges = 0usize;
    let (mut engine_clean, mut engine_leaks) = (0usize, 0usize);
    let (mut route_clean, mut route_leaks) = (0usize, 0usize);
    for path in &corpus() {
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let expect = base.get(&name).unwrap_or(&Expect::Clean).clone();
        let stdin_fixture = path.with_extension("stdin");
        let prog_args = read_args(&path.with_extension("args"));

        // The embedded engine: `vyrn run` compiles and runs in one process, so
        // `VYRN_LEAK_CHECK` selects the accounting allocator and arms the run
        // at once.
        let mut cmd = vyrn();
        cmd.env("VYRN_LEAK_CHECK", "1");
        cmd.arg("run").arg(path).args(&prog_args);
        let r = run_io(cmd, &dir, &stdin_fixture);
        let err = norm(&r.stderr);
        let before = failures.len();
        judge(
            "engine",
            &name,
            &expect,
            r.status.code(),
            &err,
            &mut failures,
            &mut nudges,
        );
        if r.status.code() == Some(135) {
            engine_leaks += 1;
        } else if failures.len() == before {
            engine_clean += 1;
        }

        if !route {
            continue;
        }
        let exe = out_dir.join(format!("{name}.exe"));
        let build = vyrn()
            .env("VYRN_LEAK_CHECK", "1")
            .arg("build")
            .arg(path)
            .arg("-o")
            .arg(&exe)
            .output()
            .expect("build");
        if !build.status.success() {
            failures.push(format!(
                "{name} (route): an audited build failed:\n{}{}",
                norm(&build.stdout),
                norm(&build.stderr)
            ));
            continue;
        }
        let mut cmd = Command::new(&exe);
        cmd.args(&prog_args);
        let r = run_io(cmd, &dir, &stdin_fixture);
        let err = norm(&r.stderr);
        let before = failures.len();
        judge(
            "route",
            &name,
            &expect,
            r.status.code(),
            &err,
            &mut failures,
            &mut nudges,
        );
        if r.status.code() == Some(135) {
            route_leaks += 1;
        } else if failures.len() == before {
            route_clean += 1;
        }
    }
    if nudges > 0 {
        eprintln!("ratchet: {nudges} row(s) can tighten");
    }
    eprintln!(
        "\nresidue: engine {engine_clean} clean, {engine_leaks} leaking; \
         route {route_clean} clean, {route_leaks} leaking; {} failed",
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "the residue ratchet slipped:\n{}",
        failures.join("\n")
    );
}
