//! `vyrn emit-lowered`, blessed (RFC-0101 §2.7, §6.5).
//!
//! The format promises nothing — it prints a version line and rustc's answer to
//! the stability question: stability is a blessed snapshot, not a contract. A
//! format change is then one wide, reviewable diff inside the pull request that
//! makes it, instead of a compatibility argument.
//!
//! Two examples, not the corpus: ten small snapshots are read and 161 large ones
//! are skipped, and M1 needs the dump gated rather than exhaustively pinned.
//! `fib.vyrn` is the whole grammar of a body in 29 lines; `option.vyrn` carries
//! the sum types, the `match` and the `?`.
//!
//! Re-bless with `VYRN_BLESS=1 cargo test -p vyrn-cli --test lowered_dump`, and
//! read the diff before committing it — that is the whole point of the file.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    let mut d = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    d.pop();
    d.pop();
    d
}

/// The dump names the file it was asked about, so the command runs from the
/// repository root and is given a relative path: the snapshot must not carry
/// the machine it was blessed on.
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
    // The first differing line, not both transcripts — the failure output
    // convention `reproducible.rs` set and the parity harness was fixed to.
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

/// The third, added by M4: neither of the two above owns any heap, so neither
/// prints a `release` line and the placement would ship ungated. This one
/// declares `impl Owned` twice and binds both, so every release kind the phase
/// places has a blessed line.
#[test]
fn ownedcontainer_lowers_to_its_blessed_dump() {
    check("examples/ownedcontainer.vyrn", "ownedcontainer.lowered");
}

/// The fourth, added by M4's second phase, for the reason the third exists one
/// exit kind over: `ownedcontainer.vyrn` reaches a block exit and a `return`
/// and nothing else, so `break`, `continue`, `?` and the temporary a construct
/// owns would ship with no blessed line. This one reaches all six, and the
/// handover is in it as an ABSENCE — `overHandover` has a `kept` step and no
/// `exit=scrutinee` line, because an arm took the payload.
#[test]
fn releaseacrossexit_lowers_to_its_blessed_dump() {
    check(
        "examples/releaseacrossexit.vyrn",
        "releaseacrossexit.lowered",
    );
}

/// The fifth, added by M6's second phase, and it is a gate on the SHARING
/// rather than on the format.
///
/// A `place at` is inlined at its access site, so the nodes under `call @at`
/// are nodes the source does not contain. Until the driver opened a
/// [`vyrn_frontend::project::Memo`] the checker `vyrn_lower::lower` runs
/// expanded one tree and the lowering's own walk expanded another, and the
/// second one asked `Recorded` for a type at an address the first one's dead
/// tree had been freed from — so this dump printed `var w : String` for a
/// `Window`. Nothing in the other four reaches a projection, so nothing here
/// held it. A dump that renders an expansion is the cheapest thing that does.
#[test]
fn projection_lowers_to_its_blessed_dump() {
    check("examples/projection.vyrn", "projection.lowered");
}

/// Every corpus program's lowering, one sha256 a line — the pin for what the
/// loader's walks over a body PRODUCE.
///
/// RFC-0125 §3 M6 states the scope-aware body walk once, and the failure mode of
/// that slice is a silent name-resolution change: a call that resolves to
/// another module's like-named export, an argument the namespace pass forgets to
/// drop, a local that stops shadowing a renamed decl. None of those need be a
/// diagnostic, so the whole-stderr `vyrn check` diff can be byte-identical while
/// the linked program is different. `emit-lowered` prints the root module's
/// lowered body with every name RESOLVED, so a hash of it moves the moment any
/// of those does.
///
/// It prints rather than asserts, exactly as
/// `symbols_api::the_pinned_columns_over_the_corpus` does: the licence is two
/// runs of it, before and after, compared line for line. Every `.vyrn` file
/// under `examples/`, `site/`, `std/` and `compiler/vyrn-cli/tests/` is a root
/// here, so a module that is only ever imported is still hashed once as itself.
/// A program `vyrn check` refuses has no lowering, and its row records the exit
/// code instead — a refusal that appears or disappears moves this pin too.
///
/// `cargo test -p vyrn-cli --test lowered_dump -- --ignored --nocapture
/// the_pinned_lowering_over_the_corpus`
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
                // Twice, because two programs do not lower to the same bytes
                // twice (RFC-0125 §3 M6 found this with the pin's first run):
                // `std/von.vyrn` and `std/vyx.vyrn` print their `release (taken
                // later)` lines in a different ORDER on every run. That is the
                // release placement's, not the loader's, and a pin that recorded
                // one of the orders would report a difference at every run. A row
                // that does not reproduce within the run is recorded as
                // `unstable` instead, so the pin says which programs it cannot
                // speak for.
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
