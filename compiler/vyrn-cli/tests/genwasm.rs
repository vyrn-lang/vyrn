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
