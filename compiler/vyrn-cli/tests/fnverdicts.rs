//! The census of the per-function verdicts (`vyrn_lower::verdict`) over the
//! corpus roots under `examples/`, `std/` and `site/`, pinned in
//! `tests/pins/fn-verdicts.tsv`: each function a root declares, with its
//! trap-free verdict and first cause and its kernel verdict and first rule,
//! each without its line so an edit that only moves lines moves no row. A
//! root that does not check is a row of its own. The run prints the counts
//! by cause and rule, and how many functions only a 64-bit integer keeps
//! from being a kernel.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use vyrn_lower::verdict::{Cause, Scalar, Trap, Unfit};

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

/// One census row, its count keys (the trap-free kind and the kernel
/// kind), and whether only a 64-bit integer keeps it from being a kernel.
type Row = (String, [&'static str; 2], bool);

/// The census rows of the root `rel`, under its project's `artifacts` map
/// as `vyrn check` loads it.
fn rows_of(rel: &str) -> Vec<Row> {
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
        let refused = "does not check";
        return vec![(
            format!("{rel}\t\t{refused}\t{refused}"),
            [refused; 2],
            false,
        )];
    };
    let wide = vyrn_lower::verdict::table(&program, &world, true);
    (vyrn_lower::verdict::table(&program, &world, false).into_iter())
        .zip(wide)
        .map(|(v, w)| {
            let name = &program.functions[v.id.index()].name;
            let trap = (v.trap.as_ref()).map_or("yes".into(), |c| format!("no, {}", c.text(false)));
            let kernel =
                (v.kernel.as_ref()).map_or("yes".into(), |u| format!("no, {}", u.text(false)));
            let keys = [trap_kind(&v.trap), kernel_kind(&v.kernel)];
            let only_wide = v.kernel.is_some() && w.kernel.is_none();
            (format!("{rel}\t{name}\t{trap}\t{kernel}"), keys, only_wide)
        })
        .collect()
}

/// The count key of a trap-free verdict: `yes`, or its cause's kind.
fn trap_kind(cause: &Option<Cause>) -> &'static str {
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

/// The count key of a kernel verdict: `yes`, or its first rule.
fn kernel_kind(unfit: &Option<Unfit>) -> &'static str {
    match unfit {
        None => "yes",
        Some(Unfit::Effects(_)) => "effects",
        Some(Unfit::Traps) => "not trap-free",
        Some(Unfit::Recursive) => "recursive",
        Some(Unfit::Type(.., Scalar::Wide)) => "64-bit integer",
        Some(Unfit::Type(.., Scalar::Float32)) => "Float32",
        Some(Unfit::Type(.., Scalar::Float64)) => "Float64",
        Some(Unfit::Type(.., Scalar::Fits | Scalar::Other)) => "other type",
        Some(Unfit::Loop(_)) => "loop",
        Some(Unfit::Callee(..)) => "callee",
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
    let mut done: Vec<(usize, Vec<Row>)> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                // A debug build lowers the deepest roots past the default
                // thread stack; `effects` and `coretables` reserve the same.
                let deep =
                    std::thread::Builder::new().stack_size(vyrn_frontend::trap::DEEP_STACK_BYTES);
                deep.spawn_scoped(s, || {
                    let mut out = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(rel) = rels.get(i) else { break };
                        out.push((i, rows_of(rel)));
                    }
                    out
                })
                .expect("a census worker starts")
            })
            .collect();
        (workers.into_iter())
            .flat_map(|w| w.join().expect("a census worker finishes"))
            .collect()
    });
    done.sort_by_key(|(i, _)| *i);
    let rows: Vec<Row> = done.into_iter().flat_map(|(_, r)| r).collect();
    let mut counts: [BTreeMap<&str, usize>; 2] = Default::default();
    for (_, keys, _) in &rows {
        for (c, k) in counts.iter_mut().zip(keys) {
            *c.entry(k).or_default() += 1;
        }
    }
    let wide = rows.iter().filter(|r| r.2).count();
    eprintln!("trap-free {:#?}", counts[0]);
    eprintln!("kernel {:#?}", counts[1]);
    eprintln!("only a 64-bit integer: {wide}");
    let rows = rows.into_iter().map(|(r, ..)| r);
    common::pin("fn-verdicts", "root\tfunction\ttrap-free\tkernel", rows);
}
