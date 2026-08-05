//! RFC-0077 M6 — the wasm target reclaims what it allocates.
//!
//! Three-way parity cannot see this. Reclaiming memory is not observable in
//! output, which is exactly why an allocator that never freed survived every
//! milestone of RFC-0077 and three separate notes describing one face of it each.
//! `memory.buffer.byteLength` is the only thing that can see it, and reading that
//! means a host that keeps the instance alive across many calls — Node, the same
//! shape as `web/`.
//!
//! **It asserts a relation, not a number.** Memory after N calls equals memory
//! after 4N. A byte count would break on every allocator change; the relation
//! survives them, and it is the property that matters: the steady state is
//! bounded, not merely small. The repo writes tests this way elsewhere —
//! `take(unfold(..), n)` pins `n`, and RFC-0082's quadratic pins compare 40x400
//! against 10x1600.
//!
//! It runs the REAL `web/wasi-min.js`, copied to a `.mjs` so Node reads it as the
//! ES module it is. That is deliberate: the JS half of the fix is there — the
//! caller owns a `String` argument (RFC-0012), so the page has to hand it back
//! through `__vyrn_free`, and a test against a private loader would not check it.
//!
//! Skips, loudly, when `node` is absent. Same posture as the parity harness with
//! wasmtime.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel)
}

fn find_node() -> Option<PathBuf> {
    let node = std::env::var("VYRN_NODE").unwrap_or_else(|_| "node".into());
    Command::new(&node).arg("--version").output().ok().filter(|o| o.status.success())?;
    Some(PathBuf::from(node))
}

/// The measurement's shape, from RFC-0077 M6: an exported function taking a
/// `String` the JS caller allocated inside the module, plus a `String` the module
/// allocates and drops on its own.
///
/// Both halves are needed. The argument is the host's leak — `own` keys droppable
/// on `Stmt::Let`, so a parameter is borrowed and the caller frees it — and
/// `echo` is the module's, reclaimed by the block-exit release M6 taught this
/// backend to emit.
const FIXTURE: &str = r#"let mut seen: Int64 = 0

export extern fn absorb(arg: String) {
    let echo = arg + "!"
    seen = seen + Int64(echo.byteLength)
}

fn main() -> Int64 {
    return 0
}
"#;

const DRIVER: &str = r#"import { readFile } from "node:fs/promises";
import { runVyrn } from "./wasi-min.mjs";

const bytes = await readFile(new URL("./mem.wasm", import.meta.url));
const arg = "x".repeat(900);

// A fresh instance per run, so the two answers are two steady states rather than
// one run's tail.
async function after(n) {
  const { exports, memory } = await runVyrn(bytes, {});
  for (let i = 0; i < n; i++) exports.absorb(arg);
  return memory.buffer.byteLength;
}

const n = Number(process.argv[2]);
console.log(await after(n));
console.log(await after(4 * n));
"#;

#[test]
fn the_wasm_heap_reaches_a_steady_state() {
    let Some(node) = find_node() else {
        eprintln!("NOTE: no node — RFC-0077 M6's reclamation is unverified on this machine");
        return;
    };
    let dir = std::env::temp_dir().join(format!("vyrn-m6-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("mem.vyrn"), FIXTURE).unwrap();
    std::fs::write(dir.join("drive.mjs"), DRIVER).unwrap();
    std::fs::copy(repo("web/wasi-min.js"), dir.join("wasi-min.mjs")).unwrap();

    let build = Command::new(env!("CARGO_BIN_EXE_vyrn"))
        .arg("build")
        .arg(dir.join("mem.vyrn"))
        .args(["--target", "wasm", "-o"])
        .arg(dir.join("mem.wasm"))
        .output()
        .expect("vyrn build");
    assert!(
        build.status.success(),
        "build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    // 5000 and 20000 — the RFC's own measurement is the second of the two, where
    // 900-byte arguments grew `domdemo.wasm` from 2 pages to 277.
    let n = 5000;
    let out = Command::new(&node)
        .arg(dir.join("drive.mjs"))
        .arg(n.to_string())
        .output()
        .expect("node");
    assert!(out.status.success(), "node failed:\n{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    let sizes: Vec<u64> =
        text.split_whitespace().map(|s| s.parse().expect("a byte count")).collect();
    assert_eq!(sizes.len(), 2, "expected two byte counts, got {text:?}");

    assert_eq!(
        sizes[0], sizes[1],
        "the wasm heap grew with the call count: {} bytes after {n} calls, {} after {}. \
         Four times the work must cost the same memory — a difference means something \
         allocated is never handed back.",
        sizes[0],
        sizes[1],
        4 * n
    );
    let _ = std::fs::remove_dir_all(&dir);
}
