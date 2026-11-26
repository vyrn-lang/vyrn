//! `vyrn why --proofs` over the corpus roots under `examples/`, `std/` and
//! `site/`: per root, the checks the traced walk proves equal the checks the
//! emitter removes, and no premise is untraced (`vyrn_lower::elide::proofs`).
//! The traced walk also panics where its verdict differs from the decided one.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use vyrn_frontend::core::check::Verdict;
use vyrn_frontend::core::{rows, St};
use vyrn_lower::facts::Premise;

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

/// Where the root `rel`'s proofs and its removed checks disagree: a body
/// whose removed checks number other than its proved sites, or a proof with
/// an untraced premise. Empty for a root that does not check.
fn faults_of(rel: &str) -> Vec<String> {
    let path = repo_root().join(rel);
    let src = std::fs::read_to_string(&path).expect("the root reads");
    let manifest = vyrn_frontend::manifest::find(path.parent().expect("a file has a directory"));
    let mut opts = vyrn_frontend::loader::LoadOptions {
        std_root: Some(slash(&repo_root().join("std"))),
        expansions: vyrn_frontend::project::Expansions::shared(),
        ..Default::default()
    };
    if let Ok(Some(m)) = manifest {
        m.load_options(&mut opts);
    }
    let engine = vyrn_genwasm::engine();
    let loaded = vyrn_lower::load_warned(
        &src,
        &slash(&path),
        &opts,
        &vyrn_frontend::loader::DiskResolver,
        Some(&*engine),
    );
    let Ok((program, world)) = loaded.0 else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (_, body) in vyrn_lower::insight::root_frames(&program, &world) {
        let Some(id) = body.id else { continue };
        let removed = (rows(&body.stmts))
            .filter(|(s, _)| matches!(s, St::Check(c) if c.verdict == Verdict::Proved))
            .count();
        let proofs = world.proofs(id);
        let mut sites: Vec<_> = proofs.iter().map(|p| p.site).collect();
        sites.dedup();
        if sites.len() != removed {
            out.push(format!(
                "{rel} {}: {removed} removed, {} proved",
                body.name,
                sites.len()
            ));
        }
        for p in &proofs {
            if p.tree.iter().any(|l| l.src.kind == Premise::Untraced) {
                out.push(format!(
                    "{rel} {}: line {} untraced",
                    body.name, p.site.line
                ));
            }
        }
    }
    out
}

#[test]
#[ignore = "walks every corpus root; run explicitly: cargo test -p vyrn-cli --test whyproofs -- --ignored"]
fn every_removed_check_of_the_corpus_has_a_traced_proof() {
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
    let mut faults: Vec<String> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                let deep =
                    std::thread::Builder::new().stack_size(vyrn_frontend::trap::DEEP_STACK_BYTES);
                deep.spawn_scoped(s, || {
                    let mut out = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(rel) = rels.get(i) else { break };
                        out.extend(faults_of(rel));
                    }
                    out
                })
                .expect("a worker starts")
            })
            .collect();
        (workers.into_iter())
            .flat_map(|w| w.join().expect("a worker finishes"))
            .collect()
    });
    faults.sort();
    assert!(
        faults.is_empty(),
        "{} faults:\n{}",
        faults.len(),
        faults.join("\n")
    );
}
