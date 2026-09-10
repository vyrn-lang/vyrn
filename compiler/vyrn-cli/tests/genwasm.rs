//! RFC-0076 — the wasm generation engine, which is the only one.
//!
//! **This suite was differential and is not any more.** Every assertion here ran
//! the same generator under two engines and compared the bytes; the second one
//! was the tree-walker behind `VYRN_NO_WASM_GEN=1`, and RFC-0125 §3 M5 deleted
//! it. Comparing the engine with itself would be a gate that cannot fail, which
//! is the class this RFC exists to remove — so what each test states now is the
//! rule it was always about:
//!
//!   - **what a generator emits**, asserted against the fragment the case exists
//!     for (`gendemo` really read a file; the seeded accumulator's document is
//!     intact);
//!   - **what a generator's failure reads**, asserted against the canonical
//!     wording rather than against a second engine's copy of it;
//!   - **that the engine is deterministic**, run twice with a cold cache and
//!     required to answer the same bytes — which is the half of the differential
//!     property that survives one engine.
//!
//! What each generator's output MEANS is checked where it always was:
//! `tests/fixtures.rs` runs every example against its recorded stdout, and a
//! generator that emitted different source changes that.
//!
//! They need no clang and no wasi sysroot: RFC-0076 M7 emits the generator's
//! module directly.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_file(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
        .canonicalize()
        .unwrap()
}

/// Every example that imports through a generator call — `import ... from
/// gen(...)`, as opposed to `from "some/path"`. Both spellings count: the named
/// form and `import * as ns from tw(...)` (RFC-0027), which is how `twdemo`
/// reaches `std/tw` and which a corpus written by hand would have missed.
///
/// Discovered rather than listed. A hard-coded corpus is a gate that keeps
/// passing after it stops looking at the thing that changed, and the example
/// tree grows faster than anyone remembers to edit a constant.
fn generator_examples() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "vyrn") && imports_from_a_generator(&p) {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    walk(&repo_file("examples"), &mut out);
    out
}

/// A run's stderr without the `VYRN_GENWASM_TRACE` lines, so the two engines'
/// diagnostics can be compared even though only one of them was traced.
fn without_trace(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .replace("\r\n", "\n")
        .lines()
        .filter(|l| !l.starts_with("genwasm "))
        .collect::<Vec<_>>()
        .join("\n")
}

fn imports_from_a_generator(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    text.lines().any(|l| {
        let l = l.trim_start();
        l.starts_with("import ")
            && l.split_once(" from ")
                .is_some_and(|(_, src)| !src.starts_with('"'))
    })
}

/// `emit-gen <file>`, with the on-disk generator cache off so a second run
/// cannot be a cache hit answering for the first.
fn emit_gen(file: &Path) -> std::process::Output {
    emit_gen_traced(file, false)
}

/// As above, plus `VYRN_GENWASM_TRACE` — which puts per-phase lines on stderr,
/// so only the caller that reads them (and not the ones asserting stderr trap
/// wording) may ask for it.
fn emit_gen_traced(file: &Path, trace: bool) -> std::process::Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vyrn"));
    c.env("VYRN_NO_GEN_CACHE", "1");
    if trace {
        c.env("VYRN_GENWASM_TRACE", "1");
    }
    c.arg("emit-gen").arg(file).output().expect("emit-gen")
}

/// The generator, run twice with a cold output cache, required to answer the
/// same bytes. This is the half of the old differential property that survives
/// one engine: a generation that depends on anything but its declared inputs
/// shows up here.
fn emit_gen_twice(file: &Path) -> std::process::Output {
    let first = emit_gen(file);
    let again = emit_gen(file);
    assert_eq!(
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&again.stdout),
        "{}: two cold runs emitted different source",
        file.display()
    );
    assert_eq!(
        first.status.code(),
        again.status.code(),
        "{}: two cold runs exited differently",
        file.display()
    );
    first
}

/// The M2 acceptance case: `palette` reads a file AND lists a directory, both
/// mediated, so it exercises every host import the engine has.
#[test]
fn a_read_and_list_generator_emits_what_it_read() {
    let demo = repo_file("examples/gendemo.vyrn");
    let out = emit_gen_twice(&demo);
    assert!(out.status.success());
    // The generator really did read: without the mediated `readFile`/`listDir`
    // the counts would be the empty-input defaults.
    assert!(String::from_utf8_lossy(&out.stdout).contains("return \"dark.txt\""));
}

/// The M3a acceptance cases: both generators build their output with RFC-0054
/// code quotes, so they exercise `@codeText`, `@codeSplice` in expression
/// position, `Code + Code` and `render` — every operation on a handle except
/// `rawAt`. `std/tw` bakes a ~30 KB stylesheet through one splice, which is the
/// escaping the host must own.
#[test]
fn code_quote_generators_emit_the_same_source_twice() {
    for demo in ["examples/twdemo.vyrn", "examples/i18ndemo.vyrn"] {
        let f = repo_file(demo);
        let out = emit_gen_twice(&f);
        assert!(out.status.success(), "{demo} failed to generate");
    }
}

/// The M3b acceptance cases: `std/vyx` reaches `lex`, and the reflection
/// generators reach `moduleInterface` and `contractOf` — the three builtins that
/// hand back a value of a known named type. `shelf/server` runs nine generator
/// calls across `std/vyx`, `std/rpc` and `std/ui`, so it exercises the encoder
/// over `Array<Token>`, `ModuleInterface` and `ContractInfo` in one process.
#[test]
fn structured_result_generators_emit_the_same_source_twice() {
    for demo in [
        "examples/vyxdemo.vyrn",
        "examples/rpc.vyrn",
        "examples/shelf/server.vyrn",
    ] {
        let f = repo_file(demo);
        let out = emit_gen_twice(&f);
        assert!(out.status.success(), "{demo} failed to generate");
    }
}

/// `moduleInterface` records EVERY module its link touched (RFC-0031), which is
/// what makes editing a closure type's defining file miss the generator cache
/// even though its path was never a generator argument. A stale hit is worse
/// than a slow generator, and the cache entry IS the read list — so the entry is
/// read, required to name the closure file, and required to change when that
/// file does.
#[test]
fn the_reflected_type_closure_is_a_cache_input() {
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
    let out = emit_gen_twice(&main);
    assert!(out.status.success());
    // The closure type's source really did travel: without the link, `Wire`
    // (declared in another file) would not be in the interface at all.
    assert!(String::from_utf8_lossy(&out.stdout).contains("export type Wire = { n: Int64 }"));

    // With the on-disk cache ON, the entry file IS the recorded read list, so
    // editing `wire.vyrn` must change it.
    //
    // The cache is this test's OWN directory, not `~/.vyrn/cache/gen`. Reading
    // every entry in the shared one made this row depend on state it does not
    // own: any sibling test that generates writes an entry there, so the
    // comparison read somebody else's work and the row failed under parallel
    // load while passing alone.
    let cache = dir.join("gen-cache");
    let cached = || -> String {
        let mut c = Command::new(env!("CARGO_BIN_EXE_vyrn"));
        c.env("VYRN_GEN_CACHE_DIR", &cache);
        assert!(c
            .arg("emit-gen")
            .arg(&main)
            .output()
            .unwrap()
            .status
            .success());
        let mut entries: Vec<String> = std::fs::read_dir(&cache)
            .unwrap()
            .filter_map(|e| std::fs::read_to_string(e.unwrap().path()).ok())
            .collect();
        entries.sort();
        entries.join("\n---\n")
    };
    let before = cached();
    assert!(
        before.contains("wire.vyrn"),
        "the closure file is not a cache input: {before}"
    );

    std::fs::write(
        dir.join("wire.vyrn"),
        "export type Wire = { n: Int64, extra: String }\n",
    )
    .unwrap();
    assert_ne!(
        before,
        cached(),
        "editing a file in the reflected type closure was a stale cache hit"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The structured builtins are lowered ONLY on the generation path. In the
/// language they stay comptime-only, and an ordinary build still says so — the
/// `gen_host` flag must not leak a runtime meaning into a normal compile.
///
/// RFC-0096 M3 (lane C) moved WHERE it says so. All three refusals now come from
/// the checker, in the one sentence `lex` already used, so `vyrn check` and
/// `vyrn run` refuse the same call the same way — and the direct backend's
/// `no lowering for the call` is unreachable for them.
#[test]
fn reflection_outside_a_generator_is_still_the_same_error() {
    for (src, want) in [
        (
            "fn main() -> Int64 { let i = moduleInterface(\"./x\") return 0 }",
            "`moduleInterface` is only available during generation",
        ),
        (
            "contract C { fn g() -> Int64 }\nfn main() -> Int64 { let c = contractOf(C) return 0 }",
            "`contractOf` is only available during generation",
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
        let out = Command::new(env!("CARGO_BIN_EXE_vyrn"))
            .arg("build")
            .arg(&f)
            .output()
            .unwrap();
        assert!(!out.status.success(), "{src} compiled");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(want),
            "unexpected failure for {src}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = std::fs::remove_file(&f);
    }
}

/// `listDir` is the one the checker does NOT gate: the native backend refuses
/// it in a user's sentence, and the wasm target builds it.
///
/// It has a runtime under `vyrn run`, which lists the real filesystem
/// (`list_dir_is_not_generation_only`), so the front end cannot refuse the call
/// the way it refuses the three above. The emitter lowers it over `fd_readdir`
/// (RFC-0125 §3 M5; `examples/listdir.vyrn` pins the output), and since the
/// native route IS that module through wasm2c (RFC-0125 §2.5) both targets
/// build it.
///
/// It was `list_dir_is_refused_natively_and_built_for_wasm`: the textual route
/// had no lowering and said so in a constant of its own. The route went and took
/// both the refusal and the constant with it, which is one example off
/// `NATIVE_UNSUPPORTED` and onto every gate the rest of the corpus is on.
#[test]
fn list_dir_builds_for_both_targets() {
    let f = std::env::temp_dir().join(format!("vyrn_listdir_{}.vyrn", std::process::id()));
    std::fs::write(
        &f,
        "fn main() -> Int64 {\n\
         \x20   let e = listDir(\".\")\n\
         \x20   return 0\n\
         }\n",
    )
    .unwrap();
    let wasm = f.with_extension("wasm");
    let built = Command::new(env!("CARGO_BIN_EXE_vyrn"))
        .arg("build")
        .arg(&f)
        .args(["--target", "wasm", "-o"])
        .arg(&wasm)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "the wasm target refused `listDir`: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    let _ = std::fs::remove_file(&f);
    let _ = std::fs::remove_file(&wasm);
}

/// A value that has no splice rule in its hole's position aborts generation with
/// the RFC-0054 message — the host applies the rule, so a refusal is a trap out
/// of `_start` and never a value the generator could swallow. Also the only
/// coverage of a hole in IDENTIFIER position, which the two code-quote
/// generators in the repo do not use.
#[test]
fn a_splice_with_no_rule_traps() {
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
    let out = emit_gen_twice(&main);
    assert!(
        !out.status.success(),
        "the invalid identifier should have failed"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not a valid non-keyword identifier"),
        "unexpected failure: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// An accumulator whose FIRST value came out of a CALL, beside a second one
/// spliced into a code quote.
///
/// `note` returns `""` — a data-segment literal — through a call the plan says
/// transfers, so the accumulator's append shadow started at "this buffer is
/// mine" and the first `+` grew the literal IN PLACE, over the next literal's
/// header in the string pool. The header that literal then carried sent the
/// next concatenation copying megabytes out of the data segment, which is how
/// `std/graphql`'s `sdl` — written in exactly this shape — came back with its
/// report spliced into the middle of the document and repeated four times in
/// front of it. The flag is advisory now and the all-ones capacity decides
/// (`std/runtime.vyrn`'s `strAppend`, and its copy in the textual backend).
#[test]
fn an_accumulator_seeded_by_a_call_does_not_grow_a_literal_in_place() {
    let dir = std::env::temp_dir().join(format!("vyrn_m5_seeded_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("gen.vyrn"),
        "fn note(tag: String) -> String {\n\
         \x20   if tag == \"loud\" {\n\
         \x20       return \"// loud\\n\"\n\
         \x20   }\n\
         \x20   return \"\"\n\
         }\n\
         export gen fn mk(tag: String) -> String {\n\
         \x20   let mut doc = \"# head\\n\"\n\
         \x20   let mut notes = note(tag)\n\
         \x20   doc = doc + \"type a\\n\"\n\
         \x20   notes = notes + \"// the note\\n\"\n\
         \x20   notes = notes + note(tag)\n\
         \x20   return notes + render(vyrn\"export fn text() -> String { return \\{doc} }\")\n\
         }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.vyrn"),
        "import { mk } from \"./gen\"\n\
         import { text } from mk(\"quiet\")\n\
         fn main() -> Int64 { print(text()) return 0 }\n",
    )
    .unwrap();

    let main = dir.join("main.vyrn");
    let got = emit_gen_twice(&main);
    assert!(
        got.status.success(),
        "generation failed:\n{}",
        String::from_utf8_lossy(&got.stderr)
    );
    let out = String::from_utf8_lossy(&got.stdout).to_string();
    // The document is the whole point: it is the accumulator's neighbour in the
    // string pool, and it came back with the note in it.
    assert!(
        out.contains("return \"# head\\ntype a\\n\""),
        "the spliced document is not intact:\n{out}"
    );
    assert_eq!(
        out.matches("// the note").count(),
        1,
        "one note, once:\n{out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `Code` is lowered ONLY on the generation path. In the language it is still
/// comptime-only, and an ordinary build still says so — the `gen_host` flag must
/// not leak a runtime meaning into a normal compile.
#[test]
fn a_code_quote_outside_a_generator_is_still_the_same_error() {
    let f = std::env::temp_dir().join(format!("vyrn_m3a_{}.vyrn", std::process::id()));
    std::fs::write(
        &f,
        "fn f() -> String {\n    return render(vyrn\"fn x() -> Int64 { return 1 }\")\n}\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_vyrn"))
        .arg("build")
        .arg(&f)
        .output()
        .unwrap();
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
/// swallow.
#[test]
fn a_read_outside_the_declared_inputs_traps() {
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
    let out = emit_gen_twice(&main);
    assert!(
        !out.status.success(),
        "the escaping read should have failed"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("escapes its declared inputs"),
        "unexpected failure: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A generator that fails on its own terms — an index past the end, a division
/// by zero — reads with the language's own trap wording and nothing else. It
/// nearly did not: the compiled runtime prefixes a trap with `error: ` on its
/// way to stderr, which is right at the top level (the CLI prints that prefix
/// for a program's trap, and parity compares them) and wrong here, where the
/// loader supplies the context and wants a bare message.
#[test]
fn a_generator_trap_reads_with_the_language_wording() {
    let dir = std::env::temp_dir().join(format!("vyrn_m5_traps_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Each body computes its failing value through a loop, so the failure is a
    // runtime trap in the generator rather than something const-folded away.
    std::fs::write(
        dir.join("gen.vyrn"),
        "export gen fn oob(tag: String) -> String {\n\
         \x20   let xs = [1, 2, 3]\n\
         \x20   let mut i = 0\n\
         \x20   while i < 10 { i = i + 1 }\n\
         \x20   return \"export fn n() -> Int64 { return \" + xs[i].toString() + \" }\"\n\
         }\n\
         export gen fn dz(tag: String) -> String {\n\
         \x20   let mut i = 0\n\
         \x20   while i < 3 { i = i + 1 }\n\
         \x20   return \"export fn n() -> Int64 { return \" + (10 / (i - 3)).toString() + \" }\"\n\
         }\n\
         export gen fn si(tag: String) -> String {\n\
         \x20   let mut i = 0\n\
         \x20   while i < 99 { i = i + 1 }\n\
         \x20   return \"export fn n() -> Int64 { return \" + tag[i].toString() + \" }\"\n\
         }\n",
    )
    .unwrap();
    for (g, want) in [
        ("oob", "array index 10 out of bounds"),
        ("dz", "division by zero"),
        ("si", "string index 99 out of bounds"),
    ] {
        let main = dir.join(format!("{g}.vyrn"));
        std::fs::write(
            &main,
            format!(
                "import {{ {g} }} from \"./gen\"\n\
                 import {{ n }} from {g}(\"x\")\n\
                 fn main() -> Int64 {{ print(n()) return 0 }}\n"
            ),
        )
        .unwrap();
        let out = emit_gen(&main);
        assert!(!out.status.success(), "{g} should have trapped");
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        assert!(err.contains(want), "{g}: unexpected failure: {err}");
        // The bare message, not the top level's. `error: ` in front of a
        // generator's trap is the prefix this test was written to catch.
        assert!(
            !err.contains(&format!("error: {want}")),
            "{g}: the top-level prefix reached a generator's trap: {err}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The milestone's reason to exist (RFC-0076 M5). A runaway generator has to
/// fail loudly; before fuel metering it ran forever — and since M4 that means it
/// hung the editor.
///
/// Both halves matter: the message is the language's, and the run must actually
/// END. A test that hangs on regression is a test that failed.
#[test]
fn a_runaway_generator_is_killed() {
    let dir = std::env::temp_dir().join(format!("vyrn_m5_runaway_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("gen.vyrn"),
        // The append is unreachable, and there so the loop cannot be optimized
        // away as having no effect: what is being metered is the spinning.
        // The bound is 10^12 rather than a merely large number, and RFC-0076 M7 is
        // why: the fuel budget is `1000 * the interpreter's step budget`, and how
        // far a loop gets inside it depends on how many wasm instructions the
        // backend spends per Vyrn statement. The direct backend spends about 14
        // where clang at `-O0` spent enough to exceed it, so at 10^9 this generator
        // COMPLETED under wasm while still dying under the interpreter — the test
        // was measuring the emitter's efficiency, not the guardrail. Past any
        // multiplier this mapping could plausibly take, it measures the guardrail.
        "export gen fn spin(tag: String) -> String {\n\
         \x20   let mut i = 0\n\
         \x20   let mut s = \"\"\n\
         \x20   while i < 1000000000000 {\n\
         \x20       i = i + 1\n\
         \x20       if i < 0 { s = s + tag }\n\
         \x20   }\n\
         \x20   return \"export fn n() -> Int64 { return 1 }\"\n\
         }\n",
    )
    .unwrap();
    let main = dir.join("main.vyrn");
    std::fs::write(
        &main,
        "import { spin } from \"./gen\"\n\
         import { n } from spin(\"x\")\n\
         fn main() -> Int64 { print(n()) return 0 }\n",
    )
    .unwrap();

    let out = emit_gen(&main);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("generator exceeded its step budget"),
        "unexpected failure: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The compiled artifact is cached on disk across sessions (RFC-0076 M5), keyed
/// on the content hashes of the generator's own module closure. So the thing
/// worth testing is not the hit — every other test here is one — but the MISS:
/// editing the generator, or a file it imports, must never be answered by the
/// artifact compiled from the old text. A stale artifact is a silently wrong
/// program, which is worse than any amount of clang.
#[test]
fn editing_a_generator_recompiles_its_artifact() {
    let dir = std::env::temp_dir().join(format!("vyrn_m5_artifact_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // The number the generator emits comes from a module the generator IMPORTS,
    // so only a fingerprint over the whole closure notices the edit.
    std::fs::write(
        dir.join("part.vyrn"),
        "export fn v() -> Int64 { return 1 }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("gen.vyrn"),
        "import { v } from \"./part\"\n\
         export gen fn mk(tag: String) -> String {\n\
         \x20   return \"export fn n() -> Int64 { return \" + v().toString() + \" }\"\n\
         }\n",
    )
    .unwrap();
    let main = dir.join("main.vyrn");
    std::fs::write(
        &main,
        "import { mk } from \"./gen\"\n\
         import { n } from mk(\"x\")\n\
         fn main() -> Int64 { print(n()) return 0 }\n",
    )
    .unwrap();

    // `VYRN_NO_GEN_CACHE` turns off the OUTPUT cache, not the artifact cache, so
    // a stale artifact would show through as stale generated source.
    let before = emit_gen(&main);
    assert!(before.status.success());
    assert!(String::from_utf8_lossy(&before.stdout).contains("return 1"));

    std::fs::write(
        dir.join("part.vyrn"),
        "export fn v() -> Int64 { return 2 }\n",
    )
    .unwrap();
    let after = emit_gen(&main);
    assert!(after.status.success());
    assert!(
        String::from_utf8_lossy(&after.stdout).contains("return 2"),
        "a stale compiled artifact answered for an edited generator: {}",
        String::from_utf8_lossy(&after.stdout)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// RFC-0076's own acceptance criterion, over the CORPUS rather than the cases
/// anyone thought of: *every* generator-using example in the repo emits the same
/// source twice from a cold cache, and the engine really ran for each of them.
///
/// It used to compare two engines. RFC-0125 §3 M5 left one, so the second column
/// is the SAME engine on a second cold run — which still catches a generator
/// whose answer depends on anything but its declared inputs, and is the only
/// half of the differential property one engine can carry. What the emitted
/// source MEANS is checked by `tests/fixtures.rs`, which runs every one of these
/// examples against its recorded output.
///
/// The engine could decline a generator and used to fall through to the
/// tree-walker; there is nothing to fall through to now, so a decline is a
/// refusal. `VYRN_GENWASM_TRACE` is still how a run is proved to have happened,
/// and every successful engine run prints a `run` phase — a file with no
/// `genwasm run:` line is a FAILURE here even though its two runs agree.
///
/// Ignored: compiles every generator in the repo twice. ~20 s in release with an
/// empty artifact cache; ~100 s in debug, because the guest is compiled by
/// cranelift and a debug cranelift is the whole difference. Run it in release.
#[test]
#[ignore = "compiles every generator in the corpus twice: cargo test -p vyrn-cli --test genwasm -- --ignored"]
fn every_generator_example_emits_the_same_source_twice() {
    let corpus = generator_examples();
    assert!(
        corpus.len() >= 10,
        "generator corpus looks wrong: {corpus:?}"
    );

    let mut failures: Vec<String> = Vec::new();
    let root = repo_file("examples");
    for path in &corpus {
        let name = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .display()
            .to_string();
        let first = emit_gen_traced(path, true);
        let again = emit_gen(path);
        let f_err = String::from_utf8_lossy(&first.stderr).to_string();
        let ran = f_err.matches("genwasm run:").count();

        if first.status != again.status {
            failures.push(format!(
                "{name}: exit {:?} then {:?}\n{f_err}",
                first.status.code(),
                again.status.code()
            ));
        } else if first.stdout != again.stdout {
            failures.push(format!("{name}: two cold runs emitted different source"));
        } else if !first.status.success() {
            // A REFUSAL is an observation too. Since RFC-0099 a generator may
            // report an error of its own, so an example that does not build is a
            // normal corpus citizen — as long as it refuses in the same words
            // twice. The traced run's own lines are dropped first; only the
            // caller that asked for them reads them.
            let a = without_trace(&first.stderr);
            let b = without_trace(&again.stderr);
            if a == b {
                eprintln!("OK  {name}  ({ran} generator calls compiled; refused the same way)");
                continue;
            }
            failures.push(format!(
                "{name}: the refusal moved between runs\n  first: {a}\n  again: {b}"
            ));
        } else if ran == 0 {
            failures.push(format!(
                "{name}: the engine never ran, so nothing here was generated\n{f_err}"
            ));
        } else {
            eprintln!("OK  {name}  ({ran} generator calls compiled)");
            continue;
        }
        for line in f_err.lines().filter(|l| l.starts_with("genwasm declined:")) {
            eprintln!("  {name}: {line}");
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
