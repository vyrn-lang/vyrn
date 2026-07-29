//! RFC-0076 — the wasm generation engine, checked against the interpreter it
//! replaces.
//!
//! Both assertions are differential: the same generator, run under both engines,
//! must produce the same bytes. `VYRN_NO_WASM_GEN=1` forces the interpreter, so
//! each test compares the engine against the reference rather than against a
//! transcript nobody would notice going stale.
//!
//! These are meaningful only when the binary is built with `--features
//! wasm-gen` (and with a wasi sysroot present); without it both runs are the
//! interpreter and the tests pass by agreeing with themselves. That is
//! deliberate — the engine is optional, and a test that failed without it would
//! make the default build red.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_file(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel).canonicalize().unwrap()
}

/// `emit-gen <file>`, with the on-disk generator cache off so the second run
/// cannot be a cache hit answering for the first.
fn emit_gen(file: &Path, wasm: bool) -> std::process::Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vyrn"));
    c.env("VYRN_NO_GEN_CACHE", "1");
    if !wasm {
        c.env("VYRN_NO_WASM_GEN", "1");
    }
    c.arg("emit-gen").arg(file).output().expect("emit-gen")
}

/// The M2 acceptance case: `palette` reads a file AND lists a directory, both
/// mediated, so it exercises every host import the engine has.
#[test]
fn read_and_list_generators_emit_the_same_source_under_both_engines() {
    let demo = repo_file("examples/gendemo.vyrn");
    let interp = emit_gen(&demo, false);
    let wasm = emit_gen(&demo, true);
    assert!(interp.status.success() && wasm.status.success());
    assert_eq!(
        String::from_utf8_lossy(&interp.stdout),
        String::from_utf8_lossy(&wasm.stdout),
        "the wasm engine's emitted source diverged from the interpreter's"
    );
    // The generator really did read: without the mediated `readFile`/`listDir`
    // the counts would be the empty-input defaults.
    assert!(String::from_utf8_lossy(&wasm.stdout).contains("return \"dark.txt\""));
}

/// A read outside the generator's declared inputs aborts generation with the
/// scoping trap — it must never reach the generator as an `Err` value it could
/// swallow, under either engine.
#[test]
fn a_read_outside_the_declared_inputs_traps_identically() {
    let dir = std::env::temp_dir().join(format!("vyrn_genwasm_escape_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("data")).unwrap();
    std::fs::write(dir.join("secret.txt"), "shh").unwrap();
    std::fs::write(dir.join("data/ok.txt"), "fine").unwrap();
    std::fs::write(
        dir.join("gen.vyrn"),
        "export gen fn peek(dir: String) -> String {\n\
         \x20   let s = match readFile(dir + \"/../secret.txt\") { Ok(t) => t, Err(e) => \"\" }\n\
         \x20   return \"export fn n() -> Int64 { return \" + s.byteLength.toString() + \" }\"\n\
         }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.vyrn"),
        "import { peek } from \"./gen\"\n\
         import { n } from peek(\"./data\")\n\
         fn main() -> Int64 { print(n()) return 0 }\n",
    )
    .unwrap();

    let main = dir.join("main.vyrn");
    let interp = emit_gen(&main, false);
    let wasm = emit_gen(&main, true);
    assert!(!interp.status.success(), "the escaping read should have failed");
    assert_eq!(
        String::from_utf8_lossy(&interp.stderr),
        String::from_utf8_lossy(&wasm.stderr),
        "the wasm engine's trap wording diverged from the interpreter's"
    );
    assert!(
        String::from_utf8_lossy(&wasm.stderr).contains("escapes its declared inputs"),
        "unexpected failure: {}",
        String::from_utf8_lossy(&wasm.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}
