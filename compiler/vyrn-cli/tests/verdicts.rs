//! Holds the check verdicts `vyrn emit-lowered` prints equal to the ones the
//! emitted program acts on, for every example the wasm manifest builds. The
//! emitted side is the oracle build's `vyrn:sites` section (`VYRN_CHECKS=<file>`),
//! which names every check row the emitter met with its verdict.
//!
//! A body the emitter did not reach is in no site row, and is skipped.

mod common;
use common::*;
use std::collections::{BTreeMap, BTreeSet};

/// `(line, ordinal, rule, verdict)` per body, `verdict` as the oracle words it.
type Rows = BTreeMap<String, BTreeSet<(u32, u32, String, String)>>;

/// The check rows of an `emit-lowered` dump, by the body that prints them.
fn printed(dump: &str) -> Rows {
    let mut rows = Rows::new();
    let mut body = String::new();
    for l in dump.lines() {
        if let Some(head) = l.strip_prefix("fn ") {
            body = head[..head.rfind('(').expect("a body's head has parameters")].to_string();
            continue;
        }
        let w: Vec<&str> = l.split_whitespace().collect();
        let verdict = match w.first() {
            Some(&"proved") => "proved",
            Some(&"check") => "kept",
            _ => continue,
        };
        let at = l
            .rsplit_once("(line ")
            .expect("a check row ends in its site")
            .1;
        let (line, ordinal) = at.trim_end_matches(')').split_once(" #").unwrap();
        let row = (
            line.parse().unwrap(),
            ordinal.parse().unwrap(),
            w[1].to_string(),
            verdict.to_string(),
        );
        rows.entry(body.clone()).or_default().insert(row);
    }
    rows
}

/// The site rows of an oracle build: the counter table's address, then one
/// `body line ordinal rule verdict` row per site.
fn emitted(sites: &str) -> Rows {
    let mut rows = Rows::new();
    for l in sites.lines().skip(1) {
        let f: Vec<&str> = l.split('\t').collect();
        let row = (
            f[1].parse().unwrap(),
            f[2].parse().unwrap(),
            f[3].to_string(),
            f[4].to_string(),
        );
        rows.entry(f[0].to_string()).or_default().insert(row);
    }
    rows
}

#[test]
#[ignore = "builds every example to wasm; run explicitly: cargo test -p vyrn-cli --test verdicts -- --ignored"]
fn every_printed_verdict_is_the_emitted_one() {
    let dir = examples_dir();
    let out = scratch("verdicts");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "vyrn"))
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .filter(|n| !EXPECTED_CHECK_FAILURE.iter().any(|(r, ..)| r == n))
        .collect();
    names.sort();
    let (mut bodies, mut failures) = (0, Vec::new());
    for name in &names {
        let dump = vyrn()
            .current_dir(&dir)
            .args(["emit-lowered", name])
            .output()
            .unwrap();
        assert!(dump.status.success(), "{name}: {}", norm(&dump.stderr));
        let module = out.join(format!("{name}.wasm"));
        let build = vyrn()
            .current_dir(&dir)
            .args(["build", name, "--target", "wasm", "-o"])
            .arg(&module)
            .env("VYRN_CHECKS", out.join("counts.tsv"))
            .output()
            .unwrap();
        assert!(build.status.success(), "{name}: {}", norm(&build.stderr));
        let bytes = std::fs::read(&module).unwrap();
        let sites = vyrn_cli::wasmrun::custom_section(&bytes, "vyrn:sites").unwrap_or_default();
        let ran = emitted(&sites);
        for (body, rows) in printed(&norm(&dump.stdout)) {
            let Some(got) = ran.get(&body) else { continue };
            bodies += 1;
            if *got != rows {
                failures.push(format!(
                    "{name} `{body}`:\n  printed {rows:?}\n  emitted {got:?}"
                ));
            }
        }
    }
    assert!(bodies > 0, "no printed body was emitted");
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
