//! One source builds to one artifact, every time, in every process.
//!
//! RFC-0010 fetches `github:`, `gist:` and `https:` modules and pins each one by
//! the sha256 of its bytes, and `vyrn.lock` is that argument written down. A
//! compiler that cannot reproduce its own output undermines the same argument
//! one layer up: a build nobody can repeat is a build nobody can check.
//!
//! The defect this file was written for was one `HashSet` — the module-state
//! `String` accumulators of `global_append_candidates`, ITERATED by the direct
//! backend to reserve one ownership word each. A reservation is an address baked
//! into every `i32.const` that reads or writes it, and it shifts every later
//! reservation, so the whole static map moved with it: `region_sp`, `call_depth`,
//! the region vectors, the free-list heads. `RandomState` is seeded per process,
//! so one accumulator was stable by accident, two were a coin flip, and the three
//! below built SIX different modules from this one file — same length, first
//! difference at byte 1016, inside the code section.
//!
//! Two things follow about the shape of this test.
//!
//! It has to cross a PROCESS boundary. A `HashSet` iterates identically twice
//! inside one process, so a test that builds twice in-process is green while the
//! defect is live. Each row here spawns the compiler afresh, `n` times.
//!
//! And it compares BYTES rather than auditing containers. The container was the
//! cause once; the property is that no container may decide the output, and only
//! the artifact can say whether one does.

use std::path::PathBuf;
use std::process::Command;

/// How many separate compilers have to agree. With three accumulators there are
/// six orders, so seven independent runs agreeing by luck is about one in
/// 300,000 — and the failure is not flaky in the other direction: a
/// deterministic compiler passes every time.
const RUNS: usize = 7;

/// A program with several module-state `String` accumulators — the shape that
/// makes the defect visible — plus a generic and a stored `fn` value, so a
/// worklist or a registry that grew an unordered container is caught here too.
///
/// The accumulators must be grown by `g = g + …` and read only through positions
/// that cannot retain the pointer (an interpolation copies), or the whitelist in
/// `global_append_candidates` bans them and no ownership word is reserved at all
/// — which is exactly how the first attempt at this file passed while the defect
/// was live.
const SRC: &str = r#"let mut alpha: String = ""
let mut beta: String = ""
let mut gamma: String = ""

fn twin<T>(x: T) -> Array<T> {
    return [x, x]
}

fn grow(s: String) -> Int64 {
    alpha = alpha + s
    beta = beta + s + s
    gamma = gamma + s + s + s
    return 0
}

fn apply(f: fn(Int64) -> Int64, n: Int64) -> Int64 {
    return f(n)
}

fn main() -> Int64 {
    grow("\{twin(1).length}")
    grow("\{twin(2.5).length}")
    grow("\{twin(true).length}")
    let twice: fn(Int64) -> Int64 = n -> n + n
    print("\{alpha}\{beta}\{gamma}\{apply(twice, 3)}")
    return 0
}
"#;

fn dir() -> PathBuf {
    let d = std::env::temp_dir().join("vyrn-reproducible");
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The source on disk, written once per row so two rows cannot race on a path.
fn source(name: &str) -> PathBuf {
    let f = dir().join(format!("{name}.vyrn"));
    std::fs::write(&f, SRC).unwrap();
    f
}

fn vyrn() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vyrn"))
}

/// Fail with the run that disagreed and where, rather than with "not equal".
fn all_equal(runs: &[Vec<u8>], what: &str) {
    let first = &runs[0];
    for (i, r) in runs.iter().enumerate().skip(1) {
        if r == first {
            continue;
        }
        let at = first
            .iter()
            .zip(r.iter())
            .position(|(a, b)| a != b)
            .map(|p| p.to_string())
            .unwrap_or_else(|| format!("byte {} — the outputs are different lengths", first.len()));
        panic!(
            "{what}: run 1 and run {} of the SAME source disagree at byte {at} \
             ({} bytes vs {} bytes).\n  \
             note: every run is a fresh process, so a container seeded per process — \
             a `HashSet` or `HashMap` whose iteration order reaches an address, a name \
             or an index — is what this row catches\n  \
             note: the fix is an ordered container (`BTreeSet`/`BTreeMap`) or a sort \
             before the loop that emits",
            i + 1,
            first.len(),
            r.len()
        );
    }
}

/// The wasm backend, which is where the defect was: `RANDOM` addresses in the
/// static map, from `RandomState` alone.
#[test]
fn the_same_source_builds_to_the_same_wasm_bytes_in_every_process() {
    let src = source("repro");
    let outs: Vec<Vec<u8>> = (0..RUNS)
        .map(|i| {
            let out = dir().join(format!("repro{i}.wasm"));
            let _ = std::fs::remove_file(&out);
            let o = vyrn()
                .args(["build", &src.display().to_string(), "--target", "wasm"])
                .arg("-o")
                .arg(&out)
                .output()
                .expect("vyrn build --target wasm");
            assert!(
                o.status.success(),
                "run {i} did not build:\n{}{}",
                String::from_utf8_lossy(&o.stderr),
                String::from_utf8_lossy(&o.stdout)
            );
            std::fs::read(&out).expect("the build wrote a module")
        })
        .collect();
    all_equal(&outs, "wasm");
}

/// The lowered form's text (RFC-0101 §2.7), which lands with this row rather
/// than after it.
///
/// The dump is a printer over containers a lowering fills, and the lowering
/// keeps two `HashMap`s of its own — the checker's per-node answers and the
/// per-instance substitution. Neither may reach an ORDER, which is the exact
/// mistake the module comment above records. Zig's `--verbose-air` is the
/// cautionary precedent for the other half of the same discipline: a dump
/// checked by nothing has broken three times in its issue tracker, and this
/// repository already deleted a whole backend at `b1eef04` for going unbuilt and
/// unnoticed for twelve days.
#[test]
fn the_same_source_lowers_to_the_same_text_in_every_process() {
    let src = source("repolower");
    let outs: Vec<Vec<u8>> = (0..RUNS)
        .map(|_| {
            let o = vyrn()
                .args(["emit-lowered", &src.display().to_string()])
                .output()
                .expect("vyrn emit-lowered");
            assert!(
                o.status.success(),
                "emit-lowered failed:\n{}",
                String::from_utf8_lossy(&o.stderr)
            );
            assert!(!o.stdout.is_empty(), "emit-lowered printed nothing");
            o.stdout
        })
        .collect();
    all_equal(&outs, "lowered");
}

/// The textual backend, which was deterministic through the same defect — it
/// reads the same set for MEMBERSHIP only, inside a walk over `program.globals`
/// in declaration order, and names the flag symbolically. That asymmetry is why
/// nobody noticed for as long as nobody did, so the honest gate covers both.
#[test]
fn the_same_source_emits_the_same_ir_in_every_process() {
    let src = source("reproir");
    let outs: Vec<Vec<u8>> = (0..RUNS)
        .map(|_| {
            let o = vyrn()
                .args(["emit-ir", &src.display().to_string()])
                .output()
                .expect("vyrn emit-ir");
            assert!(
                o.status.success(),
                "emit-ir failed:\n{}",
                String::from_utf8_lossy(&o.stderr)
            );
            o.stdout
        })
        .collect();
    all_equal(&outs, "llvm ir");
}
