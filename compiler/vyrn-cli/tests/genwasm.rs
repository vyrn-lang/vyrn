//! Tests the wasm generation engine: what a generator emits, the
//! wording of its failures, and that two cold runs emit the same bytes. What the
//! output means is checked by `tests/fixtures.rs`. No clang or wasi sysroot is
//! needed: the generator's module is emitted directly.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_file(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
        .canonicalize()
        .unwrap()
}

/// Every example that imports from a generator call, named or `import * as ns`
/// (`twdemo`). Discovered rather than listed, so a new example is never missed.
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

/// A run's stderr without the `VYRN_GENWASM_TRACE` lines, so a traced run's
/// diagnostics compare with an untraced one's.
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

/// As above; `trace` adds per-phase lines to stderr, so a caller that asserts
/// stderr wording must pass false.
fn emit_gen_traced(file: &Path, trace: bool) -> std::process::Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vyrn"));
    c.env("VYRN_NO_GEN_CACHE", "1");
    if trace {
        c.env("VYRN_GENWASM_TRACE", "1");
    }
    c.arg("emit-gen").arg(file).output().expect("emit-gen")
}

/// Runs the generator twice with a cold output cache and requires the same bytes:
/// a generation that depends on anything but its declared inputs shows up here.
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

/// `moduleInterface` records every module its link touched, so editing
/// a closure type's file misses the generator cache though it was never an
/// argument. The cache entry is the read list, so the test reads it.
#[test]
fn the_reflected_type_closure_is_a_cache_input() {
    let dir = std::env::temp_dir().join(format!("vyrn_m3b_reads_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // `wire` is reached only through `api`'s imports, so only the closure walk
    // can put it in the cache key.
    std::fs::write(dir.join("wire.vyrn"), "export type Wire = { n: Int64 }\n").unwrap();
    std::fs::write(
        dir.join("api.vyrn"),
        "import { Wire } from \"./wire\"\nexport fn ping(w: Wire) -> Int64 { return w.n }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("gen.vyrn"),
        "export gen fn stub(path: String) -> Code {\n\
         \x20   let iface = moduleInterface(path)\n\
         \x20   let mut out = vyrn\"\"\n\
         \x20   for f in iface.functions {\n\
         \x20       let arity = name(f.name + \"Arity\")\n\
         \x20       out = out + vyrn\"export fn \\{arity}() -> Int64 { return \\{f.params.length} }\\n\"\n\
         \x20   }\n\
         \x20   for t in iface.types { out = out + vyrn\"// \\{t.source}\\n\" }\n\
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
    assert!(String::from_utf8_lossy(&out.stdout).contains("export type Wire = { n: Int64 }"));

    // A private cache directory, not `~/.vyrn/cache/gen`: sibling tests write
    // entries to the shared one, and this row failed under parallel load.
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

/// The `gen_host` flag must not leak a runtime meaning into a normal compile. The
/// checker states all three refusals, so `vyrn check` and `vyrn run` agree.
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

/// The host applies the splice rule, so a refusal is a trap out of
/// `_start`, never a value the generator could swallow. The only coverage of a
/// hole in identifier position.
#[test]
fn a_splice_with_no_rule_traps() {
    let dir = std::env::temp_dir().join(format!("vyrn_m3a_splice_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("gen.vyrn"),
        "export gen fn mk(fname: String) -> Code {\n\
         \x20   return vyrn\"export fn \\{name(fname)}() -> Int64 { return 1 }\"\n\
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
        String::from_utf8_lossy(&out.stderr).contains("`name` needs one identifier"),
        "unexpected failure: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `note` returns a data-segment literal through a call the plan says transfers.
/// If the append trusted that, the first `+` would grow the literal in place over
/// its neighbour in the string pool (`std/graphql`'s `sdl` has this shape). The
/// all-ones capacity decides instead (`std/runtime.vyrn`'s `strAppend`).
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
         export gen fn mk(tag: String) -> Code {\n\
         \x20   let mut doc = \"# head\\n\"\n\
         \x20   let mut notes = note(tag)\n\
         \x20   doc = doc + \"type a\\n\"\n\
         \x20   notes = notes + \"// the note\\n\"\n\
         \x20   notes = notes + note(tag)\n\
         \x20   return vyrn\"export fn text() -> String { return \\{doc} }\\nexport fn notes() -> String { return \\{notes} }\"\n\
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
    // The document is the accumulator's neighbour in the string pool.
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

/// The `gen_host` flag must not leak a runtime meaning into a normal compile.
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

/// The escape must never reach the generator as an `Err` value it could swallow.
#[test]
fn a_read_outside_the_declared_inputs_traps() {
    let dir = std::env::temp_dir().join(format!("vyrn_genwasm_escape_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("data")).unwrap();
    std::fs::write(dir.join("secret.txt"), "shh").unwrap();
    std::fs::write(dir.join("data/ok.txt"), "fine").unwrap();
    std::fs::write(
        dir.join("gen.vyrn"),
        "export gen fn peek(dir: String) -> Code {\n\
         \x20   let s = match readFile(dir + \"/../secret.txt\") { Ok(t) => t, Err(e) => \"\" }\n\
         \x20   return vyrn\"export fn n() -> Int64 { return \\{s.byteLength} }\"\n\
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

/// The runtime prefixes a trap with `error: `, which is right for a program and
/// wrong here, where the loader supplies the context and wants a bare message.
#[test]
fn a_generator_trap_reads_with_the_language_wording() {
    let dir = std::env::temp_dir().join(format!("vyrn_m5_traps_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Each failing value comes through a loop, so it is not const-folded away.
    std::fs::write(
        dir.join("gen.vyrn"),
        "export gen fn oob(tag: String) -> Code {\n\
         \x20   let xs = [1, 2, 3]\n\
         \x20   let mut i = 0\n\
         \x20   while i < 10 { i = i + 1 }\n\
         \x20   let v = xs[i]\n\
         \x20   return vyrn\"export fn n() -> Int64 { return \\{v} }\"\n\
         }\n\
         export gen fn dz(tag: String) -> Code {\n\
         \x20   let mut i = 0\n\
         \x20   while i < 3 { i = i + 1 }\n\
         \x20   let v = 10 / (i - 3)\n\
         \x20   return vyrn\"export fn n() -> Int64 { return \\{v} }\"\n\
         }\n\
         export gen fn si(tag: String) -> Code {\n\
         \x20   let mut i = 0\n\
         \x20   while i < 99 { i = i + 1 }\n\
         \x20   let v = tag[i].toString()\n\
         \x20   return vyrn\"export fn n() -> Int64 { return \\{v.byteLength} }\"\n\
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
        assert!(
            !err.contains(&format!("error: {want}")),
            "{g}: the top-level prefix reached a generator's trap: {err}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Fuel metering: without it a runaway generator hangs the editor. A
/// regression shows as a hang.
#[test]
fn a_runaway_generator_is_killed() {
    let dir = std::env::temp_dir().join(format!("vyrn_m5_runaway_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("gen.vyrn"),
        // The unreachable append keeps the loop from being optimized away. The
        // bound is 10^12 because the fuel budget counts wasm instructions: at
        // 10^9 an efficient emitter finishes the loop inside the budget.
        "export gen fn spin(tag: String) -> Code {\n\
         \x20   let mut i = 0\n\
         \x20   let mut s = \"\"\n\
         \x20   while i < 1000000000000 {\n\
         \x20       i = i + 1\n\
         \x20       if i < 0 { s = s + tag }\n\
         \x20   }\n\
         \x20   return vyrn\"export fn n() -> Int64 { return 1 }\"\n\
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

/// The compiled artifact is cached on disk, keyed on the hashes of the
/// generator's module closure, so editing a file it imports must miss.
#[test]
fn editing_a_generator_recompiles_its_artifact() {
    let dir = std::env::temp_dir().join(format!("vyrn_m5_artifact_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // The emitted number comes from a module the generator imports.
    std::fs::write(
        dir.join("part.vyrn"),
        "export fn v() -> Int64 { return 1 }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("gen.vyrn"),
        "import { v } from \"./part\"\n\
         export gen fn mk(tag: String) -> Code {\n\
         \x20   return vyrn\"export fn n() -> Int64 { return \\{v()} }\"\n\
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

    // `VYRN_NO_GEN_CACHE` turns off the output cache, not the artifact cache.
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

/// A generator whose closure holds a grammar's parser has a cache entry and an
/// artifact: the grammar's inputs stand for the generated module, so an unchanged
/// grammar reuses both and an edited one runs and compiles again.
#[test]
fn a_generator_that_holds_a_generated_module_is_cached_under_its_inputs() {
    let dir = std::env::temp_dir().join(format!("vyrn_gen_in_gen_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("home")).unwrap();
    let grammar = dir.join("g.syntax");
    std::fs::write(
        &grammar,
        "syntax G\nexpect ^E\nDoc = w:word\nword = [a-z]+\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("gen.vyrn"),
        "import { syntax } from \"std/syntax\"\n\
         import * as g from syntax(\"./g.syntax\")\n\
         export gen fn mk() -> Code {\n\
         \x20   let n = match g.parseDoc(\"abc\") {\n\
         \x20       Ok(d) => d.w.text.byteLength,\n\
         \x20       Err(e) => 0,\n\
         \x20   }\n\
         \x20   return vyrn\"export fn n() -> Int64 { return \\{n} }\"\n\
         }\n",
    )
    .unwrap();
    let main = dir.join("main.vyrn");
    std::fs::write(
        &main,
        "import { mk } from \"./gen\"\n\
         import { n } from mk()\n\
         fn main() -> Int64 { print(n()) return 0 }\n",
    )
    .unwrap();
    let run = |output_cache: bool| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_vyrn"));
        c.env("VYRN_GENWASM_TRACE", "1")
            .env("USERPROFILE", dir.join("home"))
            .env("HOME", dir.join("home"));
        if !output_cache {
            c.env("VYRN_NO_GEN_CACHE", "1");
        }
        let out = c.arg("emit-gen").arg(&main).output().expect("emit-gen");
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let trace = String::from_utf8_lossy(&out.stderr).to_string();
        assert!(out.status.success(), "{trace}");
        (text, trace)
    };

    let (text, trace) = run(true);
    assert!(text.contains("return 3"), "{text}");
    assert!(trace.contains("genwasm cranelift"), "{trace}");
    let (_, trace) = run(true);
    assert!(!trace.contains("genwasm run:"), "no entry reused: {trace}");
    // A nested load compiles at its own depth, so the first cold run fills that key.
    run(false);
    let (text, trace) = run(false);
    assert!(text.contains("return 3"), "{text}");
    assert!(trace.contains("genwasm run:"), "{trace}");
    assert!(!trace.contains("genwasm cranelift"), "no artifact: {trace}");

    std::fs::write(
        &grammar,
        "syntax G\nexpect ^E\nDoc = w:word\nword = [a-z]\n",
    )
    .unwrap();
    let (text, trace) = run(true);
    assert!(text.contains("return 0"), "a stale parser answered: {text}");
    assert!(trace.contains("genwasm cranelift"), "{trace}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The trace proves the engine ran: a file with no `genwasm run:` line fails
/// even though its two runs agree.
///
/// Ignored: about 20 s in release, 100 s in debug, because cranelift compiles
/// the guest. Run it in release.
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
            // A generator may report an error of its own, so a
            // refusal passes if it reads the same twice.
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

/// `examples/projection_generator_site.vyrn` expands a projection site of its
/// generator's program and one of its own at one node and line. Only a cold
/// generator cache loads and checks the generator's program, so the run has
/// a private one.
#[test]
fn a_generator_programs_projection_site_expands_apart_from_its_importers() {
    let cache = std::env::temp_dir().join(format!("vyrn-projsite-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cache);
    let out = Command::new(env!("CARGO_BIN_EXE_vyrn"))
        .env("VYRN_GEN_CACHE_DIR", &cache)
        .arg("run")
        .arg(repo_file("examples/projection_generator_site.vyrn"))
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&cache);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "37\n", "{err}");
}

/// A std copy holding a generator module with one import of its own and one
/// grammar, a program that imports only the generator, and the commands over
/// them. A witness needs a module of the std tree, because a user's module links.
struct StdTool {
    dir: PathBuf,
}

impl StdTool {
    const TOOL: &'static str = "import { helpValue } from \"std/toolhelp\"\n\
         import { syntax } from \"std/syntax\"\n\
         import * as g from syntax(\"./tool.syntax\")\n\
         export gen fn mk(tag: String) -> Code {\n\
         \x20   let n = match g.parseDoc(\"abc\") {\n\
         \x20       Ok(d) => d.w.text.byteLength,\n\
         \x20       Err(e) => 0,\n\
         \x20   }\n\
         \x20   return vyrn\"export fn n() -> Int64 { return \\{n + helpValue()} }\\nexport fn tag() -> String { return \\{tag} }\\n\"\n\
         }\n\
         export fn plain() -> Int64 { return 7 }\n\
         export to \"./elsewhere\" gen fn fenced(tag: String) -> Code {\n\
         \x20   return vyrn\"export fn f() -> Int64 { return 1 }\\n\"\n\
         }\n";
    const MAIN: &'static str = "import { mk } from \"std/tool\"\n\
         import { n, tag } from mk(\"x\")\n\
         fn main() -> Int64 { print(n()) print(tag()) return 0 }\n";

    fn new(test: &str) -> Self {
        fn copy(from: &Path, to: &Path) {
            std::fs::create_dir_all(to).unwrap();
            for e in std::fs::read_dir(from).unwrap() {
                let e = e.unwrap();
                match e.file_type().unwrap().is_dir() {
                    true => copy(&e.path(), &to.join(e.file_name())),
                    false => drop(std::fs::copy(e.path(), to.join(e.file_name())).unwrap()),
                }
            }
        }
        let dir = std::env::temp_dir().join(format!("vyrn_{test}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        copy(&repo_file("std"), &dir.join("std"));
        let tool = Self { dir };
        tool.write(
            "std/toolhelp.vyrn",
            "export fn helpValue() -> Int64 { return 100 }\n",
        );
        tool.write(
            "std/tool.syntax",
            "syntax G\nexpect ^E\nDoc = w:word\nword = [a-z]+\n",
        );
        tool.write("std/tool.vyrn", Self::TOOL);
        tool.write("main.vyrn", Self::MAIN);
        tool
    }

    fn write(&self, rel: &str, text: &str) {
        std::fs::write(self.dir.join(rel), text).unwrap();
    }

    fn edit(&self, rel: &str, from: &str, to: &str) {
        let text = std::fs::read_to_string(self.dir.join(rel)).unwrap();
        assert!(text.contains(from), "{rel} holds no `{from}`");
        self.write(rel, &text.replace(from, to));
    }

    /// `vyrn <cmd> main.vyrn` with this tree's std and cache, as stdout, stderr
    /// and exit code.
    fn run(&self, cmd: &str, cached: bool) -> (String, String, Option<i32>) {
        let mut c = Command::new(env!("CARGO_BIN_EXE_vyrn"));
        c.current_dir(&self.dir)
            .env("VYRN_STD", self.dir.join("std"))
            .env("VYRN_GEN_CACHE_DIR", self.dir.join("cache"))
            .env("USERPROFILE", self.dir.join("home"))
            .env("HOME", self.dir.join("home"));
        if !cached {
            c.env("VYRN_NO_GEN_CACHE", "1");
        }
        let out = c.arg(cmd).arg("main.vyrn").output().expect("run vyrn");
        let text = |b: &[u8]| String::from_utf8_lossy(b).to_string();
        (text(&out.stdout), text(&out.stderr), out.status.code())
    }

    /// What `emit-gen` writes with a cached generation, asserted equal to what it
    /// writes with none, on `check` as well.
    fn written(&self) -> String {
        for cmd in ["emit-gen", "check"] {
            let hit = self.run(cmd, true);
            assert_eq!(hit, self.run(cmd, true), "{cmd}: a second hit moved");
            assert_eq!(hit, self.run(cmd, false), "{cmd}: a hit and a miss differ");
        }
        self.run("emit-gen", true).0
    }

    /// The refusal `check` prints, asserted equal with and without a cached
    /// generation.
    fn refusal(&self) -> String {
        let (out, err, code) = self.run("check", true);
        assert_eq!(code, Some(1), "not refused: {out}{err}");
        assert_eq!(
            (out, err.clone(), code),
            self.run("check", false),
            "a hit and a miss differ"
        );
        err
    }
}

impl Drop for StdTool {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn a_generator_imported_alone_leaves_its_module_out_of_the_program() {
    let t = StdTool::new("tool_out");
    let text = t.written();
    assert!(text.contains("return 103"), "{text}");
    assert!(!text.contains("tool.syntax"), "the module linked: {text}");
}

#[test]
fn an_edit_to_a_left_out_generator_module_reaches_the_next_compile() {
    let t = StdTool::new("tool_module");
    t.written();
    t.edit("std/tool.vyrn", "n + helpValue()", "n + helpValue() + 1000");
    assert!(t.written().contains("return 1103"));
}

#[test]
fn an_edit_to_an_import_of_a_left_out_module_reaches_the_next_compile() {
    let t = StdTool::new("tool_import");
    t.written();
    t.edit("std/toolhelp.vyrn", "return 100", "return 200");
    assert!(t.written().contains("return 203"));
}

#[test]
fn an_edit_to_a_grammar_a_left_out_module_reads_reaches_the_next_compile() {
    let t = StdTool::new("tool_grammar");
    t.written();
    t.edit("std/tool.syntax", "[a-z]+", "[a-z]");
    assert!(t.written().contains("return 100"));
}

#[test]
fn an_edit_to_the_generator_arguments_reaches_the_next_compile() {
    let t = StdTool::new("tool_args");
    t.written();
    t.edit("main.vyrn", "mk(\"x\")", "mk(\"y\")");
    assert!(t.written().contains("return \"y\""));
}

#[test]
fn a_cached_generator_whose_name_left_the_exports_is_refused() {
    let t = StdTool::new("tool_unexported");
    t.written();
    t.edit("std/tool.vyrn", "export gen fn mk", "gen fn mk");
    assert!(t.refusal().contains("mk"));
}

#[test]
fn a_cached_generator_whose_body_stopped_checking_is_refused() {
    let t = StdTool::new("tool_refused");
    t.written();
    t.edit(
        "std/tool.vyrn",
        "let n = match",
        "let z: Int64 = \"no\"\n    let n = match",
    );
    assert!(t.refusal().contains("String"));
}

#[test]
fn a_deleted_cache_entry_writes_the_same_module() {
    let t = StdTool::new("tool_deleted");
    let before = t.written();
    std::fs::remove_dir_all(t.dir.join("cache")).unwrap();
    assert_eq!(before, t.written());
}

#[test]
fn a_second_name_from_the_generator_module_links_it() {
    let t = StdTool::new("tool_second");
    t.edit("main.vyrn", "{ mk }", "{ mk, plain }");
    t.edit("main.vyrn", "print(n())", "print(n() + plain())");
    let text = t.written();
    assert!(
        text.contains("tool.syntax"),
        "the module did not link: {text}"
    );
    assert_eq!(t.run("check", true).2, Some(0));
}

#[test]
fn a_std_body_that_spells_the_generator_links_its_module() {
    let t = StdTool::new("tool_spelled");
    t.write(
        "std/toolcaller.vyrn",
        "import { mk } from \"std/tool\"\n\
         import { n } from mk(\"z\")\n\
         export fn viaBody() -> Int64 { let c = mk(\"q\") return n() }\n",
    );
    t.write(
        "main.vyrn",
        "import { viaBody } from \"std/toolcaller\"\n\
         fn main() -> Int64 { return viaBody() }\n",
    );
    assert!(t.written().contains("tool.syntax"));
}

#[test]
fn a_fenced_generator_stays_refused() {
    let t = StdTool::new("tool_fenced");
    t.write(
        "main.vyrn",
        "import { fenced } from \"std/tool\"\n\
         import { f } from fenced(\"x\")\n\
         fn main() -> Int64 { return f() }\n",
    );
    assert!(t.refusal().contains("only to"));
}

#[test]
fn a_declaration_of_the_imported_generator_name_stays_refused() {
    let t = StdTool::new("tool_declared");
    t.edit(
        "main.vyrn",
        "fn main()",
        "fn mk() -> Int64 { return 1 }\nfn main()",
    );
    assert!(t.refusal().contains("declared by both"));
}

#[test]
fn a_generator_its_module_did_not_import_runs_from_a_left_out_module() {
    let t = StdTool::new("tool_unbound");
    t.write(
        "first.vyrn",
        "import { mk } from \"std/tool\"\n\
         import { n } from mk(\"a\")\n\
         export fn one() -> Int64 { return n() }\n",
    );
    t.write(
        "second.vyrn",
        "import * as b from mk(\"b\")\n\
         export fn two() -> Int64 { return b.n() }\n",
    );
    t.write(
        "main.vyrn",
        "import { one } from \"./first\"\n\
         import { two } from \"./second\"\n\
         fn main() -> Int64 { return one() + two() }\n",
    );
    assert!(t.written().contains("return \"b\""));
}

#[test]
fn an_unreached_body_of_a_user_generator_module_stays_refused() {
    let t = StdTool::new("tool_user");
    t.write(
        "local.vyrn",
        "export gen fn mine(tag: String) -> Code {\n\
         \x20   return vyrn\"export fn n() -> Int64 { return 1 }\\n\"\n\
         }\n\
         fn broken() -> Int64 { return \"no\" }\n",
    );
    t.write(
        "main.vyrn",
        "import { mine } from \"./local\"\n\
         import { n } from mine(\"x\")\n\
         fn main() -> Int64 { return n() }\n",
    );
    assert!(t.refusal().contains("expected Int64"));
}
