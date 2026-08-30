//! The exit-residue RATCHET (RFC-0114 §25, exit-residue round twenty-five).
//!
//! Every top-level example is built natively and run once under
//! `VYRN_LEAK_CHECK=1`, and the verdict is compared against the committed
//! baseline (`rfcs/census/residue-baseline.tsv`). The rules:
//!
//!   - a DOUBLE FREE (exit 134) fails, whatever the baseline says;
//!   - a `clean` row that now leaks fails — a regression;
//!   - a `leak N` row that now leaks MORE than N blocks fails — the ratchet
//!     only turns one way;
//!   - an example with no row must come out clean — new examples do not get
//!     to leak quietly;
//!   - a `leak` row that comes out clean, or smaller, passes and says so —
//!     that is a nudge to shrink the baseline, not an error.
//!
//! `other` rows exit nonzero by design (their own exit codes are their
//! outputs); they pass as long as the leak check stays quiet. `skip` rows do
//! not build natively (hosts, serve, externs) and are expected to keep
//! failing to build — one that STARTS building must take a real row.
//!
//! Needs clang (the native column), so it is `#[ignore]` like the parity
//! harness and runs in the same CI job.

mod common;
use common::*;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, PartialEq)]
enum Expect {
    Clean,
    Leak(u64),
    Other,
    Skip,
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
            "skip" => Expect::Skip,
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

/// `free audit: N block(s), ...` — the leak check's one stderr line.
fn blocks(stderr: &str) -> Option<u64> {
    let at = stderr.rfind("free audit: ")?;
    let rest = &stderr[at + "free audit: ".len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

#[test]
#[ignore] // needs clang; run with the parity job
fn the_residue_ratchet_only_turns_one_way() {
    let base = baseline();
    let dir = examples_dir();
    let out = scratch("residue");
    let mut failures: Vec<String> = Vec::new();
    let mut nudges = 0usize;
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| {
            let p = e.unwrap().path();
            (p.extension().is_some_and(|x| x == "vyrn"))
                .then(|| p.file_stem().unwrap().to_string_lossy().into_owned())
        })
        .collect();
    names.sort();
    for name in &names {
        let expect = base.get(name).unwrap_or(&Expect::Clean);
        let src = dir.join(format!("{name}.vyrn"));
        let exe = out.join(format!("{name}.exe"));
        let built = vyrn()
            .args(["build"])
            .arg(&src)
            .arg("-o")
            .arg(&exe)
            .current_dir(&dir)
            .output()
            .expect("run vyrn build");
        if !built.status.success() {
            if *expect != Expect::Skip {
                failures.push(format!("{name}: no longer builds natively"));
            }
            continue;
        }
        if *expect == Expect::Skip {
            failures.push(format!("{name}: builds now — give it a real baseline row"));
            continue;
        }
        let mut cmd = std::process::Command::new(&exe);
        cmd.env("VYRN_LEAK_CHECK", "1").current_dir(&dir);
        let stdin_fixture = dir.join(format!("{name}.stdin"));
        if stdin_fixture.exists() {
            cmd.stdin(std::fs::File::open(&stdin_fixture).unwrap());
        } else {
            cmd.stdin(std::process::Stdio::null());
        }
        let ran = cmd.output().expect("run example");
        let code = ran.status.code();
        let err = String::from_utf8_lossy(&ran.stderr);
        match code {
            Some(134) => failures.push(format!("{name}: DOUBLE FREE\n{err}")),
            Some(135) => {
                let got = blocks(&err).unwrap_or(u64::MAX);
                match expect {
                    Expect::Leak(max) if got <= *max => {
                        if got < *max {
                            nudges += 1;
                            eprintln!(
                                "ratchet: {name} leaks {got} (baseline {max}) — shrink its row"
                            );
                        }
                    }
                    Expect::Leak(max) => failures.push(format!(
                        "{name}: residue grew — {got} block(s), baseline allows {max}"
                    )),
                    _ => failures.push(format!(
                        "{name}: new residue — {got} block(s), baseline says {expect:?}"
                    )),
                }
            }
            Some(0) | Some(_) => {
                // The program's own exit code. `other` rows exit nonzero by
                // design; a `leak` row that reaches here came out clean.
                if let Expect::Leak(_) = expect {
                    nudges += 1;
                    eprintln!("ratchet: {name} is CLEAN now — move its row to `clean`");
                }
            }
            None => failures.push(format!("{name}: killed by signal")),
        }
    }
    if nudges > 0 {
        eprintln!("ratchet: {nudges} row(s) can tighten");
    }
    assert!(
        failures.is_empty(),
        "the residue ratchet slipped:\n{}",
        failures.join("\n")
    );
}
