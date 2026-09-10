//! What the move check's walk PRODUCES, over the whole corpus — RFC-0125 §3
//! M3, the walk slice.
//!
//! The walk is 1,200 of `movecheck.rs`'s lines and it states no rule. On the
//! only mode a compile runs (`Want::Lets`) it produces three things and nothing
//! else: the arity of every lambda whose signature the declaration does not
//! name, the signature key of every lambda it does, and the projection store
//! whose desugared group the walk descends into instead of the index and the
//! value. `own::analyze` reads the first two through the fn-value meet; the
//! third builds `project`'s store memo.
//!
//! **This is the licence for rewriting that walk.** It prints, it asserts
//! nothing about the numbers, and the rule is the one
//! `symbols_api::the_pinned_columns_over_the_corpus` states: run it before and
//! after and compare every byte. Nothing else sees these rows — a lambda arity
//! moves no byte of `vyrn check` stderr until it changes a signature the meet
//! clears, and a store row moves none at all.
//!
//! It **links and checks**, for `tests/projections.rs`'s reason: a file read
//! alone cannot name an imported type, and the store rows need `project`'s memo
//! open and the checker's record made inside it.
//!
//! Ignored by default. Run it with
//! `cargo test -p vyrn-cli --test letswalk -- --ignored --nocapture`.

use std::path::{Path, PathBuf};
use vyrn_frontend::loader::DiskResolver;

#[test]
#[ignore = "links and checks the whole corpus; run explicitly"]
fn the_pinned_lets_outputs_over_the_corpus() {
    std::thread::Builder::new()
        .stack_size(vyrn_frontend::trap::DEEP_STACK_BYTES)
        .spawn(print)
        .unwrap()
        .join()
        .unwrap();
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn slashed(p: &Path) -> String {
    p.canonicalize()
        .unwrap_or_else(|e| panic!("{}: {e}", p.display()))
        .to_string_lossy()
        .replace('\\', "/")
        .replace("//?/", "")
}

/// Every `.vyrn` directly under a repo-relative directory, in sorted order —
/// the 323 roots the `vyrn check` licence runs over.
fn roots() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for rel in [
        "examples",
        "std",
        "site",
        "site/app",
        "compiler/vyrn-cli/tests/refusals",
        "compiler/vyrn-cli/tests/unlicensed",
    ] {
        let Ok(rd) = std::fs::read_dir(repo().join(rel)) else {
            continue;
        };
        let mut here: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "vyrn"))
            .collect();
        here.sort();
        out.append(&mut here);
    }
    out
}

fn print() {
    // A corpus example may import through a generator, and generation is the
    // driver's engine. Both installations are idempotent.
    vyrn_genwasm::install();
    vyrn_lower::install();

    let std_root = slashed(&repo().join("std"));
    let repo_prefix = format!("{}/", slashed(&repo()));

    let (mut walked, mut rows) = (0usize, 0usize);
    for path in roots() {
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        let root_key = slashed(&path);
        let opts = vyrn_frontend::loader::LoadOptions {
            std_root: Some(std_root.clone()),
            ..Default::default()
        };
        let Ok(program) = vyrn_frontend::loader::load(&src, &root_key, &opts, &DiskResolver) else {
            println!("===== {} unlinkable", root_key.replace(&repo_prefix, ""));
            continue;
        };
        // The store rows exist only inside a compile: `project::stored` answers
        // from a memo the compile opens, and the checker fills it as it records.
        let _memo = vyrn_frontend::project::Memo::open();
        let out = vyrn_frontend::movecheck::lets_outputs(&program);
        walked += 1;
        rows += out.len();
        println!(
            "===== {} ({} rows)",
            root_key.replace(&repo_prefix, ""),
            out.len()
        );
        for r in out {
            println!("{r}");
        }
    }
    println!("{walked} programs walked, {rows} rows");
}
