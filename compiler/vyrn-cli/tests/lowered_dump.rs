//! `vyrn emit-lowered`, blessed. The format is no contract: a
//! blessed snapshot makes a format change one reviewable diff. A few small
//! examples are snapshotted, not the corpus.
//!
//! Re-bless with `VYRN_BLESS=1 cargo test -p vyrn-cli --test lowered_dump`, and
//! read the diff before committing it.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    let mut d = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    d.pop();
    d.pop();
    d
}

/// Runs from the repository root with a relative path: the dump names its
/// file, and the snapshot must not carry the machine it was blessed on.
fn dump(example: &str) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_vyrn"))
        .current_dir(repo_root())
        .args(["emit-lowered", example])
        .output()
        .expect("vyrn emit-lowered");
    assert!(
        out.status.success(),
        "emit-lowered {example} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("the dump is UTF-8")
        .replace("\r\n", "\n")
}

fn check(example: &str, snapshot: &str) {
    let got = dump(example);
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(snapshot);
    if std::env::var("VYRN_BLESS").is_ok() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, got.as_bytes()).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| {
            panic!(
                "{}: {e}\n  note: bless it with VYRN_BLESS=1",
                path.display()
            )
        })
        .replace("\r\n", "\n");
    if got == want {
        return;
    }
    let (g, w): (Vec<&str>, Vec<&str>) = (got.lines().collect(), want.lines().collect());
    let at = g
        .iter()
        .zip(&w)
        .position(|(a, b)| a != b)
        .unwrap_or(g.len().min(w.len()));
    panic!(
        "{example}: the lowered dump changed at line {}\n  blessed: {}\n  now:     {}\n  \
         ({} lines blessed, {} now)\n  note: if the change is intended, re-bless with \
         VYRN_BLESS=1 and read the diff",
        at + 1,
        w.get(at).unwrap_or(&"<end of file>"),
        g.get(at).unwrap_or(&"<end of file>"),
        w.len(),
        g.len()
    );
}

#[test]
fn fib_lowers_to_its_blessed_dump() {
    check("examples/fib.vyrn", "fib.lowered");
}

#[test]
fn option_lowers_to_its_blessed_dump() {
    check("examples/option.vyrn", "option.lowered");
}

/// Gives every release kind a blessed line; `fib` and `option` own no heap.
#[test]
fn ownedcontainer_lowers_to_its_blessed_dump() {
    check("examples/ownedcontainer.vyrn", "ownedcontainer.lowered");
}

/// Reaches all six exit kinds, including `break`, `continue`, `?` and a
/// construct's temporary. The handover shows as an absence: `overHandover`
/// drops no scrutinee after the `switch`, because an arm took the payload.
#[test]
fn releaseacrossexit_lowers_to_its_blessed_dump() {
    check(
        "examples/releaseacrossexit.vyrn",
        "releaseacrossexit.lowered",
    );
}

/// Gates the sharing of projection expansions, not the format. The nodes under
/// `call @at` are not in the source; if the checker and the lowering expand
/// separate trees (no shared [`vyrn_frontend::project::Memo`]), `Recorded`
/// answers a type for a freed address and types a `Window` as a `String`.
#[test]
fn projection_lowers_to_its_blessed_dump() {
    check("examples/projection.vyrn", "projection.lowered");
}

/// Prints one sha256 per corpus root of its lowering, with every name resolved.
/// A silent name-resolution change need not move any diagnostic, but it moves
/// this hash.
///
/// It prints rather than asserts: the licence is a run before and after,
/// compared line for line. Every `.vyrn` file under the four directories is a
/// root, so a module that is only imported is still hashed. A refused program
/// records its exit code instead.
#[test]
#[ignore = "runs the compiler over the whole corpus; run explicitly"]
fn the_pinned_lowering_over_the_corpus() {
    let root = repo_root();
    let mut files = Vec::new();
    for dir in ["examples", "site", "std", "compiler/vyrn-cli/tests"] {
        let mut stack = vec![root.join(dir)];
        while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().and_then(|s| s.to_str()) == Some("vyrn") {
                    files.push(p);
                }
            }
        }
    }
    let mut rels: Vec<String> = files
        .iter()
        .map(|p| {
            p.strip_prefix(&root)
                .unwrap_or(p)
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    rels.sort();
    let dump = |rel: &str| {
        let out = Command::new(env!("CARGO_BIN_EXE_vyrn"))
            .current_dir(&root)
            .args(["emit-lowered", rel])
            .output()
            .expect("vyrn emit-lowered");
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"))
            .ok_or_else(|| out.status.code().unwrap_or(-1))
    };
    let (mut lowered, mut unstable) = (0usize, 0usize);
    for rel in &rels {
        match dump(rel) {
            Ok(text) => {
                lowered += 1;
                // Twice: a row that does not reproduce within the run is
                // recorded as `unstable`, so the pin names the programs it
                // cannot speak for.
                let again = dump(rel).unwrap_or_default();
                if again == text {
                    println!(
                        "{rel}\t{}\t{} lines",
                        vyrn_frontend::hash::sha256_hex(text.as_bytes()),
                        text.lines().count()
                    );
                } else {
                    unstable += 1;
                    println!("{rel}\tunstable\t{} lines", text.lines().count());
                }
            }
            Err(rc) => println!("{rel}\trefused rc={rc}"),
        }
    }
    println!(
        "===== {} programs, {lowered} lowered, {unstable} unstable",
        rels.len()
    );
}

/// Covers large roots the snapshots and the wasm manifest do not reach. A
/// placement that walks a `HashMap` reorders its `drop` rows between
/// processes (Rust seeds `RandomState` per process); ten runs catch a rare
/// reordering.
#[test]
fn the_lowering_is_the_same_bytes_on_every_run() {
    for root in [
        "std/von.vyrn",
        "std/vyx.vyrn",
        "site/app/docs.vyrn",
        "examples/regexredux.vyrn",
    ] {
        let first = dump(root);
        assert!(
            first.lines().any(|l| l.trim_start().starts_with("drop ")),
            "{root} places no release, so it gates nothing"
        );
        for run in 2..=10 {
            let again = dump(root);
            if again == first {
                continue;
            }
            let (a, b): (Vec<&str>, Vec<&str>) = (again.lines().collect(), first.lines().collect());
            let at = a
                .iter()
                .zip(&b)
                .position(|(x, y)| x != y)
                .unwrap_or(a.len().min(b.len()));
            panic!(
                "{root}: run {run} differs from run 1 at line {}\n  run 1: {}\n  run {run}: {}",
                at + 1,
                b.get(at).unwrap_or(&"<end of file>"),
                a.get(at).unwrap_or(&"<end of file>"),
            );
        }
    }
}
