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

/// The M3a acceptance cases: both generators build their output with RFC-0054
/// code quotes, so they exercise `@codeText`, `@codeSplice` in expression
/// position, `Code + Code` and `render` — every operation on a handle except
/// `rawAt`. `std/tw` bakes a ~30 KB stylesheet through one splice, which is the
/// escaping the host must own.
#[test]
fn code_quote_generators_emit_the_same_source_under_both_engines() {
    for demo in ["examples/twdemo.vyrn", "examples/i18ndemo.vyrn"] {
        let f = repo_file(demo);
        let interp = emit_gen(&f, false);
        let wasm = emit_gen(&f, true);
        assert!(interp.status.success() && wasm.status.success(), "{demo} failed to generate");
        assert_eq!(
            String::from_utf8_lossy(&interp.stdout),
            String::from_utf8_lossy(&wasm.stdout),
            "{demo}: the wasm engine's emitted source diverged from the interpreter's"
        );
    }
}

/// A value that has no splice rule in its hole's position aborts generation with
/// the RFC-0054 message, under either engine — the host applies the rule, so a
/// refusal is a trap out of `_start` and never a value the generator could
/// swallow. Also the only coverage of a hole in IDENTIFIER position, which the
/// two code-quote generators in the repo do not use.
#[test]
fn a_splice_with_no_rule_traps_identically() {
    let dir = std::env::temp_dir().join(format!("vyrn_m3a_splice_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("gen.vyrn"),
        "export gen fn mk(name: String) -> String {\n\
         \x20   return render(vyrn\"export fn \\{name}() -> Int64 { return 1 }\")\n\
         }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.vyrn"),
        "import { mk } from \"./gen\"\n\
         import { badName } from mk(\"bad-name\")\n\
         fn main() -> Int64 { print(badName()) return 0 }\n",
    )
    .unwrap();

    let main = dir.join("main.vyrn");
    let interp = emit_gen(&main, false);
    let wasm = emit_gen(&main, true);
    assert!(!interp.status.success(), "the invalid identifier should have failed");
    assert_eq!(
        String::from_utf8_lossy(&interp.stderr),
        String::from_utf8_lossy(&wasm.stderr),
        "the wasm engine's splice trap diverged from the interpreter's"
    );
    assert!(
        String::from_utf8_lossy(&wasm.stderr).contains("not a valid non-keyword identifier"),
        "unexpected failure: {}",
        String::from_utf8_lossy(&wasm.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `Code` is lowered ONLY on the generation path. In the language it is still
/// comptime-only, and an ordinary build still says so — the `gen_host` flag must
/// not leak a runtime meaning into a normal compile.
#[test]
fn a_code_quote_outside_a_generator_is_still_the_same_error() {
    let f = std::env::temp_dir().join(format!("vyrn_m3a_{}.vyrn", std::process::id()));
    std::fs::write(&f, "fn f() -> String {\n    return render(vyrn\"fn x() -> Int64 { return 1 }\")\n}\n")
        .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_vyrn")).arg("build").arg(&f).output().unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("`render` is only available during generation"),
        "unexpected failure: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_file(&f);
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
