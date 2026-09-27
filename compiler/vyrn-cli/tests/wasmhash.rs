//! Hashes the wasm the direct backend emits for every example `vyrn check`
//! accepts, against `tests/pins/wasm-sha256.tsv`. CI runs it with `check` on
//! every platform, so output that depends on the host (a `HashMap` walk, a
//! platform `usize`, a baked-in path) fails on the leg where it differs.
//!
//! `VYRN_WASM_MANIFEST`: unset writes the manifest to scratch only; `check` also
//! compares with the committed one; `write` also replaces it.
//!
//! Examples are built by bare file name from `examples/`: a generated module's
//! symbol map keys on the path the loader was given.

mod common;
use common::*;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use vyrn_frontend::hash::sha256_hex;

const HEADER: &str = "# sha256 of the wasm the direct backend emits, one row per example.\n\
                      # regenerate: VYRN_WASM_MANIFEST=write cargo test -p vyrn-cli --test wasmhash -- --ignored\n";

fn committed_manifest() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/pins/wasm-sha256.tsv")
}

/// `name -> sha256`.
fn rows(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let (name, hash) = l.split_once('\t').expect("name<TAB>sha256");
            (name.to_string(), hash.to_string())
        })
        .collect()
}

#[test]
#[ignore = "builds every example to wasm; run explicitly: cargo test -p vyrn-cli --test wasmhash -- --ignored"]
fn every_example_emits_the_recorded_wasm() {
    let mode = std::env::var("VYRN_WASM_MANIFEST").unwrap_or_default();
    assert!(
        matches!(mode.as_str(), "" | "check" | "write"),
        "VYRN_WASM_MANIFEST must be unset, `check` or `write`, not {mode:?}"
    );
    let dir = examples_dir();
    let out = scratch("wasmhash");

    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "vyrn"))
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .filter(|n| !EXPECTED_CHECK_FAILURE.iter().any(|(r, ..)| r == n))
        .collect();
    names.sort();
    assert!(!names.is_empty(), "no examples found in {}", dir.display());

    let mut got = BTreeMap::new();
    let mut failures = Vec::new();
    for name in &names {
        let module = out.join(format!("{name}.wasm"));
        let build = vyrn()
            .current_dir(&dir)
            .arg("build")
            .arg(name)
            .arg("--target")
            .arg("wasm")
            .arg("-o")
            .arg(&module)
            .output()
            .expect("build wasm");
        if !build.status.success() {
            failures.push(format!(
                "{name}: wasm build failed:\n{}",
                norm(&build.stderr)
            ));
            continue;
        }
        got.insert(name.clone(), sha256_hex(&std::fs::read(&module).unwrap()));
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));

    let mut text = HEADER.to_string();
    for (name, hash) in &got {
        text.push_str(&format!("{name}\t{hash}\n"));
    }
    std::fs::write(out.join("wasm-sha256.tsv"), &text).unwrap();

    match mode.as_str() {
        "write" => std::fs::write(committed_manifest(), &text).unwrap(),
        "check" => {
            let want =
                rows(&std::fs::read_to_string(committed_manifest()).expect("committed manifest"));
            let mut diff = Vec::new();
            for name in want
                .keys()
                .chain(got.keys())
                .collect::<std::collections::BTreeSet<_>>()
            {
                match (want.get(name), got.get(name)) {
                    (Some(w), Some(g)) if w != g => {
                        diff.push(format!("{name}: recorded {w}, emitted {g}"))
                    }
                    (Some(_), None) => diff.push(format!("{name}: recorded, not in the corpus")),
                    (None, Some(_)) => diff.push(format!("{name}: in the corpus, not recorded")),
                    _ => {}
                }
            }
            assert!(
                diff.is_empty(),
                "the emitted wasm differs from tests/pins/wasm-sha256.tsv on {} of {} examples:\n  {}\n\
                 if the direct backend changed on purpose, regenerate with VYRN_WASM_MANIFEST=write",
                diff.len(),
                got.len(),
                diff.join("\n  ")
            );
        }
        _ => {}
    }
    eprintln!("wasmhash: {} examples hashed", got.len());
}
