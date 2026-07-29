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

/// The M3b acceptance cases: `std/vyx` reaches `lex`, and the reflection
/// generators reach `moduleInterface` and `contractOf` — the three builtins that
/// hand back a value of a known named type. `shelf/server` runs nine generator
/// calls across `std/vyx`, `std/rpc` and `std/ui`, so it exercises the encoder
/// over `Array<Token>`, `ModuleInterface` and `ContractInfo` in one process.
#[test]
fn structured_result_generators_emit_the_same_source_under_both_engines() {
    for demo in ["examples/vyxdemo.vyrn", "examples/rpc.vyrn", "examples/shelf/server.vyrn"] {
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

/// `moduleInterface` records EVERY module its link touched (RFC-0031), which is
/// what makes editing a closure type's defining file miss the generator cache
/// even though its path was never a generator argument. A stale hit is worse
/// than a slow generator, so the recorded reads are compared engine to engine —
/// the cache entry IS the read list, so comparing the entries compares them.
#[test]
fn the_reflected_type_closure_is_recorded_identically_by_both_engines() {
    let dir = std::env::temp_dir().join(format!("vyrn_m3b_reads_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // `wire` is reached ONLY through `api`'s imports — never a generator
    // argument, so only the closure walk can put it in the cache key.
    std::fs::write(dir.join("wire.vyrn"), "export type Wire = { n: Int64 }\n").unwrap();
    std::fs::write(
        dir.join("api.vyrn"),
        "import { Wire } from \"./wire\"\nexport fn ping(w: Wire) -> Int64 { return w.n }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("gen.vyrn"),
        "export gen fn stub(path: String) -> String {\n\
         \x20   let iface = moduleInterface(path)\n\
         \x20   let mut out = \"\"\n\
         \x20   for f in iface.functions { out = out + \"export fn \" + f.name + \
         \"Arity() -> Int64 { return \" + f.params.length.toString() + \" }\\n\" }\n\
         \x20   for t in iface.types { out = out + \"// \" + t.source + \"\\n\" }\n\
         \x20   return out\n\
         }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.vyrn"),
        "import { stub } from \"./gen\"\n\
         import { pingArity } from stub(\"./api\")\n\
         fn main() -> Int64 { print(pingArity()) return 0 }\n",
    )
    .unwrap();

    let main = dir.join("main.vyrn");
    let interp = emit_gen(&main, false);
    let wasm = emit_gen(&main, true);
    assert!(interp.status.success() && wasm.status.success());
    assert_eq!(
        String::from_utf8_lossy(&interp.stdout),
        String::from_utf8_lossy(&wasm.stdout),
        "the reflected interface diverged between engines"
    );
    // The closure type's source really did travel: without the link, `Wire`
    // (declared in another file) would not be in the interface at all.
    assert!(String::from_utf8_lossy(&wasm.stdout).contains("export type Wire = { n: Int64 }"));

    // With the on-disk cache ON, the entry file IS the recorded read list. Both
    // engines must write the same one, and editing `wire.vyrn` must change it.
    let cached = |wasm: bool| -> String {
        let mut c = Command::new(env!("CARGO_BIN_EXE_vyrn"));
        if !wasm {
            c.env("VYRN_NO_WASM_GEN", "1");
        }
        assert!(c.arg("emit-gen").arg(&main).output().unwrap().status.success());
        let dir = dirs_gen_cache();
        let mut entries: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| std::fs::read_to_string(e.unwrap().path()).ok())
            .collect();
        entries.sort();
        entries.join("\n---\n")
    };
    let before_interp = cached(false);
    let before_wasm = cached(true);
    assert_eq!(before_interp, before_wasm, "the engines recorded different generator inputs");
    assert!(before_wasm.contains("wire.vyrn"), "the closure file is not a cache input: {before_wasm}");

    std::fs::write(dir.join("wire.vyrn"), "export type Wire = { n: Int64, extra: String }\n")
        .unwrap();
    let after_wasm = cached(true);
    assert_ne!(
        before_wasm, after_wasm,
        "editing a file in the reflected type closure was a stale cache hit"
    );
    assert_eq!(after_wasm, cached(false), "the engines diverged after the closure edit");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The on-disk generator cache, so the test above can read the recorded inputs
/// back. Mirrors the loader's own location.
fn dirs_gen_cache() -> PathBuf {
    let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap();
    PathBuf::from(home).join(".vyrn").join("cache").join("gen")
}

/// The structured builtins are lowered ONLY on the generation path. In the
/// language they stay comptime-only, and an ordinary build still says so — the
/// `gen_host` flag must not leak a runtime meaning into a normal compile.
#[test]
fn reflection_outside_a_generator_is_still_the_same_error() {
    for (src, want) in [
        (
            "fn main() -> Int64 { let i = moduleInterface(\"./x\") return 0 }",
            "`moduleInterface` is compile-time reflection",
        ),
        (
            "contract C { fn g() -> Int64 }\nfn main() -> Int64 { let c = contractOf(C) return 0 }",
            "`contractOf` is compile-time reflection",
        ),
        (
            "fn main() -> Int64 { let t = lex(\"let x = 1\") return 0 }",
            "`lex` is only available during generation",
        ),
    ] {
        let f = std::env::temp_dir().join(format!(
            "vyrn_m3b_{}_{}.vyrn",
            std::process::id(),
            want.len()
        ));
        std::fs::write(&f, src).unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_vyrn")).arg("build").arg(&f).output().unwrap();
        assert!(!out.status.success(), "{src} compiled");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(want),
            "unexpected failure for {src}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = std::fs::remove_file(&f);
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
