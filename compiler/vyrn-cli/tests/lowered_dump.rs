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
