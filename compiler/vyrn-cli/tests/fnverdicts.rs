//! The census of the per-function verdicts (`vyrn_lower::verdict`) over the
//! corpus roots under `examples/`, `std/` and `site/`, pinned in
//! `tests/pins/fn-verdicts.tsv`: each function a root declares, with its
//! trap-free verdict and first cause, the cause without its line so an edit
//! that only moves lines moves no row. A root that does not check is a row
//! of its own. The run prints the counts by cause.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use vyrn_lower::verdict::{Cause, Trap};

mod common;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Every `.vyrn` file under `dir`, recursively, but a build's `target/`.
fn roots_in(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).expect("a corpus directory reads") {
        let p = e.expect("an entry reads").path();
        if p.is_dir() && !p.ends_with("target") {
            roots_in(&p, out);
        } else if p.extension().is_some_and(|x| x == "vyrn") {
            out.push(p);
        }
    }
}

/// The census rows of the root `rel`, under its project's `artifacts` map
/// as `vyrn check` loads it, each with its count's key.
fn rows_of(rel: &str) -> Vec<(String, &'static str)> {
    let path = repo_root().join(rel);
    let src = std::fs::read_to_string(&path).expect("the root reads");
    let manifest = vyrn_frontend::manifest::find(path.parent().expect("a file has a directory"));
    let opts = vyrn_frontend::loader::LoadOptions {
        std_root: Some(slash(&repo_root().join("std"))),
        artifacts: manifest.ok().flatten().and_then(|m| m.artifacts),
        expansions: vyrn_frontend::project::Expansions::shared(),
        ..Default::default()
    };
    let engine = vyrn_genwasm::engine();
    let loaded = vyrn_lower::load_warned(
        &src,
        &slash(&path),
        &opts,
        &vyrn_frontend::loader::DiskResolver,
        Some(&*engine),
    );
    let Ok((program, world)) = loaded.0 else {
        return vec![(format!("{rel}\t\tdoes not check"), "does not check")];
    };
    (vyrn_lower::verdict::trap_free(&program, &world).into_iter())
        .map(|(id, cause)| {
            let name = &program.functions[id.index()].name;
            let said = (cause.as_ref()).map_or("yes".into(), |c| format!("no, {}", c.text(false)));
            (format!("{rel}\t{name}\t{said}"), kind(&cause))
        })
        .collect()
}

/// The count's key of a verdict: `yes`, or its cause's kind.
fn kind(cause: &Option<Cause>) -> &'static str {
    match cause.as_ref().map(|c| &c.trap) {
        None => "yes",
        Some(Trap::Explicit(Some(_))) => "explicit trap call",
        Some(Trap::Explicit(None)) => "trap row",
        Some(Trap::Kept(_)) => "kept check",
        Some(Trap::Value) => "function value",
        Some(Trap::Extern(_)) => "extern",
        Some(Trap::Unbuilt(_)) => "no body",
        Some(Trap::Callee(_)) => "callee",
    }
}

#[test]
#[ignore = "checks every corpus root; run explicitly: cargo test -p vyrn-cli --test fnverdicts -- --ignored"]
fn every_function_of_the_corpus_has_its_verdict() {
    let mut roots = Vec::new();
    for dir in ["examples", "std", "site"] {
        roots_in(&repo_root().join(dir), &mut roots);
    }
    let mut rels: Vec<String> = (roots.iter())
        .map(|p| slash(p.strip_prefix(repo_root()).expect("under the repo")))
        .collect();
    rels.sort();
    let next = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let mut done: Vec<(usize, Vec<(String, &str)>)> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut out = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(rel) = rels.get(i) else { break };
                        out.push((i, rows_of(rel)));
                    }
                    out
                })
            })
            .collect();
        (workers.into_iter())
            .flat_map(|w| w.join().expect("a census worker finishes"))
            .collect()
    });
    done.sort_by_key(|(i, _)| *i);
    let rows: Vec<(String, &str)> = done.into_iter().flat_map(|(_, r)| r).collect();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, k) in &rows {
        *counts.entry(k).or_default() += 1;
    }
    eprintln!("{counts:#?}");
    let rows = rows.into_iter().map(|(r, _)| r);
    common::pin("fn-verdicts", "root\tfunction\ttrap-free", rows);
}
