//! The RFC-0077 burndown ladder: every example through the DIRECT wasm backend,
//! and how far it got.
//!
//!     cargo test -p vyrn-cli --release --test directwasm -- --ignored --nocapture
//!
//! M2 is ~969 emit sites and will land over several milestones. This tier is how
//! that is tracked: it builds each example with `VYRN_WASM_BACKEND=direct`, runs
//! it under wasmtime, and makes exactly the comparison `parity` makes against the
//! interpreter — same corpus, same conventions, same `common` module, so the two
//! tiers cannot disagree about what "the same run" means.
//!
//! Needs only a `wasmtime` binary. No clang, no wasi sysroot, no builtins
//! archive — which is the acceptance criterion this RFC is chasing, asserted by
//! the shape of the test rather than in prose.
//!
//! # Why a list and not a count
//!
//! [`PASSING`] is a committed list of the examples that work, and the test fails
//! if any of them stops working. A committed *count* would let the set churn
//! silently: one example starts passing while another regresses and the number is
//! unchanged. The list also IS the progress report — the diff that adds a name is
//! the milestone.
//!
//! An example that passes without being listed does not fail the run; it prints
//! a line asking to be added. Failing there would make every M2b commit that
//! widens the lowering red before it is finished, which is the opposite of what a
//! burndown wants.

mod common;
use common::*;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

/// Examples the direct backend compiles and runs identically to the
/// interpreter. Grows as M2 lands; never shrinks silently.
const PASSING: &[&str] = &[
    "args.vyrn",
    "arrays.vyrn",
    // RFC-0077 M2l: the INFERRED release. `own::analyze` is now read by this
    // backend too, and its `ReleaseRef` bindings are released at block exit — a
    // million allocations through 65536 slots, which is exactly what this example
    // was written to prove.
    "autorelease.vyrn",
    "autovalidate.vyrn",
    "benching.vyrn",
    "bits.vyrn",
    "bytecount.vyrn",
    "clock.vyrn",
    // RFC-0078 M4c: the six codec builtins are `std/codecs` now, so this backend
    // compiles them as ordinary Vyrn rather than owing six hand-emitted lowerings.
    "codecbytes.vyrn",
    "consume.vyrn",
    "domdemo.vyrn",
    "ecs.vyrn",
    // Same routing, on the example that has called the codec builtins since
    // RFC-0014 — it was blocked on them and nothing else.
    "encoding.vyrn",
    "enum.vyrn",
    // RFC-0078 M3: `fromJson` is `std/jsonread` plus a per-type walk generated as
    // Vyrn, so the two `fromJson` rows RFC-0077 had left unlowered became a library
    // this backend already compiles — and with them every example whose only
    // blocker was a decode. `jsondecbytes` is the pin, failure shapes included.
    "enumarray.vyrn",
    "enumcodec.vyrn",
    "eventloop.vyrn",
    "externdemo2.vyrn",
    // RFC-0077 M2l: `Map<String, V>` (RFC-0028) — the literal, `m[k]`, `m[k] = v`,
    // `has`/`remove`/`keys`/`length`. `mapdemo` is the whole surface INCLUDING the
    // codecs, which came free: RFC-0078 made `toJson`/`fromJson` rewrites, so a
    // Map on the wire is a Map in Vyrn.
    "fieldmut.vyrn",
    "fib.vyrn",
    "files.vyrn",
    "floats.vyrn",
    "foreach.vyrn",
    // RFC-0077 M2l: the generational slot table (RFC-0004 §4) is three emitted
    // wasm functions over a lazily-allocated 65536-cell slab. `freelist` is the
    // one that proves the release fires — 100,000 allocations through the slab.
    "freelist.vyrn",
    "gendemo.vyrn",
    "generics.vyrn",
    "genref.vyrn",
    "htmltree.vyrn",
    "ifexpr.vyrn",
    "inlinewhere.vyrn",
    "jsonbytes.vyrn",
    "jsoncodec.vyrn",
    "jsondecbytes.vyrn",
    "jsonschema.vyrn",
    "linkedlist.vyrn",
    "map.vyrn",
    "mapdemo.vyrn",
    "modify.vyrn",
    "modules.vyrn",
    "namespace.vyrn",
    "numparse.vyrn",
    "option.vyrn",
    "ownership.vyrn",
    "patchdemo.vyrn",
    "protocol.vyrn",
    "record.vyrn",
    "reflection.vyrn",
    "scan.vyrn",
    "schemaimport.vyrn",
    "server.vyrn",
    "sizedints.vyrn",
    // RFC-0077 M2l: `SmallArray<T, N>` (RFC-0056). M2c refused it because its
    // first field is a length where a growable array keeps a pointer; the state
    // branch now lives in `walk`, so every element access is state-blind and only
    // the four header-mutating operations have arms of their own.
    "smallarray.vyrn",
    "statemod.vyrn",
    "strings.vyrn",
    // RFC-0078 M4c: `contains`/`startsWith`/`endsWith` are `std/strpred`. `slice`
    // and `byteLength` did not move, and both already had a lowering here.
    "strpredbytes.vyrn",
    "tagged.vyrn",
    "templates.vyrn",
    "testing.vyrn",
    "tree.vyrn",
    "utility.vyrn",
    // The one example whose refinement is VIOLATED at runtime, so it is the one
    // that proves the checks are emitted rather than that the bytes agree: a
    // lowering that emits the type and forgets the check passes every other
    // refinement example and fails this one (RFC-0077 M2d).
    "validate_fail.vyrn",
    // The fallible form of the same construction (RFC-0077 M2k), and the one
    // example that proves a refinement's failure is a `None` rather than the trap
    // `validate_fail.vyrn` wants: `Age?(5)` prints `-1`.
    "validate.vyrn",
    "validation.vyrn",
];

/// The same, for the shim-linked shape (RFC-0077 M2i). A superset of [`PASSING`]
/// by construction — the two emissions differ in five instructions plus whatever
/// only a shim can supply — so it is written as one, and the tier fails if a
/// standalone pass stops passing here.
///
/// **Empty, and that is the M2j result.** M2i's whole delta was RFC-0043's host
/// boundary — `clock.vyrn`, passing only because the shim defines
/// `__vyrn_now_millis` on every target. WASI defines `clock_time_get` and
/// `random_get` on every target too, so the emitted runtime reads them directly
/// and the linked shape buys nothing the standalone one does not already have.
/// Which is what M2i predicted when it found the split makes a module LARGER.
const PASSING_SHIM: &[&str] = &[];

#[test]
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test directwasm -- --ignored --nocapture"]
fn examples_through_the_direct_wasm_backend() {
    ladder("direct", PASSING);
}

/// The same corpus through the same backend, emitting the RFC-0077 M2i shape: one
/// module that imports memory and the C runtime from RFC-0076's shared shim,
/// linked at instantiation by `wasmtime --preload`.
///
/// A second tier rather than a flag on the first, for the reason this RFC keeps
/// giving: this repo has watched an ungated second backend rot to unbuildable in
/// twelve days, and a shim-importing emission that nothing runs would rot the same
/// way. It costs one more pass over 78 examples, and unlike the first tier it
/// needs clang and a wasi sysroot, because the shim is C.
#[test]
#[ignore = "needs wasmtime + clang + a wasi sysroot; run explicitly with --ignored --nocapture"]
fn examples_through_the_direct_wasm_backend_against_the_shared_shim() {
    if vyrn_codegen::toolchain::shim_wasm(false).is_none() {
        eprintln!("SKIP: no runtime shim (needs clang and a wasi sysroot)");
        return;
    }
    // Written as the delta rather than a second full list: the two emissions
    // differ in five instructions plus whatever only a shim can supply, so a
    // standalone pass that stops passing here is a regression in the LINK.
    let mut passing: Vec<&str> = PASSING.iter().chain(PASSING_SHIM).copied().collect();
    passing.sort();
    ladder("direct-shim", &passing);
}

fn ladder(backend: &str, passing: &[&str]) {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime (set VYRN_WASMTIME or unpack one under <repo>/tools/)");
        return;
    };
    let shim = backend == "direct-shim";
    let dir = examples_dir();
    let out_dir = std::env::temp_dir().join(format!("vyrn-{backend}"));
    std::fs::create_dir_all(&out_dir).unwrap();

    let mut names: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "vyrn"))
        .collect();
    names.sort();

    // Grouped by what stopped it: the emitter's gap message with its line number
    // removed, so one unimplemented construct is one entry however many sites
    // hit it.
    let mut blocked: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut passed: Vec<String> = Vec::new();
    let mut considered = 0usize;
    let mut regressions: Vec<String> = Vec::new();

    for path in &names {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        // The same three exclusions parity applies: a program that never builds,
        // or whose behaviour only a browser can supply, is not a run-time
        // comparison on any backend.
        if EXPECTED_CHECK_FAILURE.iter().chain(WASM_ONLY).chain(KNOWN_DIVERGENT).any(|(n, _)| *n == name) {
            continue;
        }
        considered += 1;

        let stdin_fixture = path.with_extension("stdin");
        let prog_args = read_args(&path.with_extension("args"));

        let module = out_dir.join(format!("{name}.wasm"));
        let _ = std::fs::remove_file(&module);
        let build = vyrn()
            .arg("build")
            .arg(path)
            .arg("--target")
            .arg("wasm")
            .arg("-o")
            .arg(&module)
            .env("VYRN_WASM_BACKEND", backend)
            .output()
            .expect("build wasm");
        if !build.status.success() {
            blocked.entry(blocker(&norm(&build.stderr))).or_default().push(name.clone());
            continue;
        }

        let mut interp_cmd = vyrn();
        interp_cmd.arg("run").arg(path).args(&prog_args);
        let interp = run_io(interp_cmd, &dir, &stdin_fixture);

        let mut wasm_cmd = Command::new(&wasmtime);
        wasm_cmd.arg("run").arg("--dir").arg(".");
        wasm_cmd.arg("--env").arg(format!("VYRN_FIXED_TIME={FIXED_TIME}"));
        wasm_cmd.arg("--env").arg(format!("VYRN_FIXED_SEED={FIXED_SEED}"));
        if shim {
            // The shim is a module, not a library: `--preload` puts it in the
            // linker under the `env` namespace the imports name, and wasmtime
            // resolves the two at instantiation.
            let side = module.with_extension("shim.wasm");
            wasm_cmd.arg("--preload").arg(format!("env={}", side.display()));
        }
        wasm_cmd.arg(&module).args(&prog_args);
        let w = run_io(wasm_cmd, &dir, &stdin_fixture);

        let (i_out, w_out) = (norm(&interp.stdout), norm(&w.stdout));
        let (i_err, w_err) = (runtime_err(&interp.stderr), runtime_err(&w.stderr));
        let (i_code, w_code) = (interp.status.code(), w.status.code());
        if i_out == w_out && i_err == w_err && i_code == w_code {
            passed.push(name.clone());
            if !passing.contains(&name.as_str()) {
                eprintln!("NEW   {name}  — passes; add it to PASSING");
            }
            continue;
        }
        // It built, so the gap is semantic rather than missing: a wrong answer
        // is a different (and worse) class of blocker than a refusal, and the
        // grouping says so.
        let detail = format!(
            "exit {i_code:?} vs {w_code:?}; stdout {i_out:?} vs {w_out:?}; \
             stderr {i_err:?} vs {w_err:?}"
        );
        blocked.entry("built, but DIVERGED from the interpreter".into()).or_default().push(name.clone());
        if passing.contains(&name.as_str()) {
            regressions.push(format!("{name}: {detail}"));
        } else {
            eprintln!("diff  {name}  {detail}");
        }
    }

    for name in passing {
        if !passed.iter().any(|p| p == name) && !regressions.iter().any(|r| r.starts_with(name)) {
            regressions.push(format!(
                "{name}: was passing, now blocked on {}",
                blocked
                    .iter()
                    .find(|(_, v)| v.iter().any(|n| n == name))
                    .map(|(k, _)| k.as_str())
                    .unwrap_or("something unreported")
            ));
        }
    }

    eprintln!("\n{backend}: {}/{considered} examples pass", passed.len());
    for (why, who) in &blocked {
        eprintln!("  {:3}  {why}", who.len());
        eprintln!("       {}", who.join(", "));
    }

    assert!(
        regressions.is_empty(),
        "examples in the {backend} PASSING list no longer pass:\n{}",
        regressions.join("\n")
    );
}

/// The one message this backend assembles at runtime rather than interning
/// whole, and the one no example reaches.
///
/// `error: array index 7 out of bounds` has the offending index in the MIDDLE,
/// so it is three writes and an `int_str` rather than a string constant — and a
/// bounds check that never fires reads exactly like one that fires with the
/// wrong wording. Both spellings, because the array and the string paths pick
/// different prefixes.
#[test]
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test directwasm -- --ignored"]
fn the_bounds_trap_says_what_the_interpreter_says() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = std::env::temp_dir().join("vyrn-directwasm-oob");
    std::fs::create_dir_all(&dir).unwrap();
    for (what, src) in [
        ("array", "fn main() -> Int64 {\n let xs: Array<Int64> = [1, 2, 3]\n print(xs[7])\n return 0\n}\n"),
        ("string", "fn main() -> Int64 {\n let s = \"hi\"\n let b = s[9]\n return 0\n}\n"),
    ] {
        let path = dir.join(format!("{what}.vyrn"));
        std::fs::write(&path, src).unwrap();
        let module = dir.join(format!("{what}.wasm"));
        let build = vyrn()
            .arg("build")
            .arg(&path)
            .arg("--target")
            .arg("wasm")
            .arg("-o")
            .arg(&module)
            .env("VYRN_WASM_BACKEND", "direct")
            .output()
            .expect("build wasm");
        assert!(build.status.success(), "{what}: {}", String::from_utf8_lossy(&build.stderr));

        let mut interp_cmd = vyrn();
        interp_cmd.arg("run").arg(&path);
        let interp = run_io(interp_cmd, &dir, &dir.join("no.stdin"));
        let mut wasm_cmd = Command::new(&wasmtime);
        wasm_cmd.arg("run").arg(&module);
        let w = run_io(wasm_cmd, &dir, &dir.join("no.stdin"));

        assert_eq!(runtime_err(&interp.stderr), runtime_err(&w.stderr), "{what}: stderr");
        assert_eq!(norm(&interp.stdout), norm(&w.stdout), "{what}: stdout");
        assert_eq!(interp.status.code(), w.status.code(), "{what}: exit");
        assert!(runtime_err(&w.stderr).contains(&format!("{what} index")), "{what}: wrong wording");
    }
}

/// Monomorphization discovering itself, which `generics.vyrn` does not do.
///
/// A wasm call names a function INDEX, so a specialization's index is handed out
/// where it is *discovered* and its body added later — which only works if the
/// two orders are the same. `twice` calls `wrap`, so `wrap<Int64>` is discovered
/// while three other specializations are still queued: a driver that drained its
/// worklist as a stack, or that appended out of turn, would renumber every call
/// after that point. The textual backend cannot notice — it emits symbols.
///
/// Also here because they are the shapes generics reach through: an aggregate
/// returned from a generic (the hidden leading destination, allocated before the
/// substitution is even solved), and a further generic solved from a generic
/// call's RESULT type — `firstOf(twice(41))` can only fix `A` if the call
/// reports `Pair<Int64, Int64>` rather than its record shape.
#[test]
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test directwasm -- --ignored"]
fn a_specialization_discovered_from_another_gets_the_index_its_callers_named() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = std::env::temp_dir().join("vyrn-directwasm-mono");
    std::fs::create_dir_all(&dir).unwrap();
    let src = "\
type Pair<A, B> = { first: A, second: B }

fn wrap<T>(x: T) -> Pair<T, T> {
    return Pair { first: x, second: x }
}

fn twice<T>(x: T) -> Pair<T, T> {
    return wrap(x)
}

fn firstOf<A, B>(p: Pair<A, B>) -> A {
    return p.first
}

fn main() -> Int64 {
    let a = twice(41)
    let b = twice(\"hi\")
    print(firstOf(a) + 1)
    print(firstOf(b))
    print(wrap(true).second)
    return firstOf(a) - 41
}
";
    let path = dir.join("mono.vyrn");
    std::fs::write(&path, src).unwrap();
    let module = dir.join("mono.wasm");
    let build = vyrn()
        .arg("build")
        .arg(&path)
        .arg("--target")
        .arg("wasm")
        .arg("-o")
        .arg(&module)
        .env("VYRN_WASM_BACKEND", "direct")
        .output()
        .expect("build wasm");
    assert!(build.status.success(), "{}", String::from_utf8_lossy(&build.stderr));

    let mut interp_cmd = vyrn();
    interp_cmd.arg("run").arg(&path);
    let interp = run_io(interp_cmd, &dir, &dir.join("no.stdin"));
    let mut wasm_cmd = Command::new(&wasmtime);
    wasm_cmd.arg("run").arg(&module);
    let w = run_io(wasm_cmd, &dir, &dir.join("no.stdin"));

    // A merged specialization is the failure this is really about: `twice<Int64>`
    // and `twice<String>` are the same source and different code, and merging
    // them prints a plausible number where a string belongs.
    assert_eq!(norm(&interp.stdout), "42\nhi\ntrue\n", "the interpreter moved");
    assert_eq!(norm(&interp.stdout), norm(&w.stdout), "stdout");
    assert_eq!(runtime_err(&interp.stderr), runtime_err(&w.stderr), "stderr");
    assert_eq!(interp.status.code(), w.status.code(), "exit");
}

/// The two `modify` shapes the corpus does not have, and one it does (RFC-0077
/// M2f).
///
/// Every `modify` parameter in `examples/` and `std/` is an **aggregate** — a
/// record, an `Array<T>`, a `Parser`, a `Scanner` — so the scalar case is
/// entirely untested by the ladder, and it is the one that needs work the
/// aggregate case does not: a scalar binding is a wasm local, which has no
/// address for the callee to write through, so the caller spills it to a frame
/// slot and reloads it after the call. Omitting either half compiles cleanly and
/// prints 21 where 42 belongs — the same silent shape M2b caught by running
/// (`modify.vyrn` printed zeroes), and the reason this exists rather than a
/// comment claiming the path is covered.
///
/// Also here: **module state as a `modify` argument**, where the address is a
/// constant rather than a frame offset, and a `modify` parameter **handed on to
/// another** one, where the address the inner call writes through is the outer
/// callee's own slot. And a global read by a later initializer, which is what
/// makes declaration order observable inside a single file.
#[test]
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test directwasm -- --ignored"]
fn a_modify_parameter_copies_back_whatever_the_caller_kept_it_in() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = std::env::temp_dir().join("vyrn-directwasm-modify");
    std::fs::create_dir_all(&dir).unwrap();
    let src = "\
type Counter = { value: Int64, hits: Int64 }

let mut base: Int64 = 7
let derived: Int64 = base * 2
let mut shared: Counter = Counter { value: 0, hits: 0 }
let label: String = \"label\"

fn twice(n: modify Int64) {
    n = n * 2
}

fn bump(c: modify Counter, by: Int64) {
    c.value = c.value + by
    c.hits = c.hits + 1
}

fn bumpTwice(c: modify Counter) {
    bump(c, 1)
    bump(c, 1)
}

fn main() -> Int64 {
    print(base)
    print(derived)
    base = base + 1
    print(base)

    let mut n = 21
    twice(n)
    print(n)

    bump(shared, 5)
    bumpTwice(shared)
    print(label)
    print(shared.value)
    print(shared.hits)

    shared.value = 100
    print(shared.value)
    return shared.hits
}
";
    let path = dir.join("modifyshapes.vyrn");
    std::fs::write(&path, src).unwrap();
    let module = dir.join("modifyshapes.wasm");
    let build = vyrn()
        .arg("build")
        .arg(&path)
        .arg("--target")
        .arg("wasm")
        .arg("-o")
        .arg(&module)
        .env("VYRN_WASM_BACKEND", "direct")
        .output()
        .expect("build wasm");
    assert!(build.status.success(), "{}", String::from_utf8_lossy(&build.stderr));

    let mut interp_cmd = vyrn();
    interp_cmd.arg("run").arg(&path);
    let interp = run_io(interp_cmd, &dir, &dir.join("no.stdin"));
    let mut wasm_cmd = Command::new(&wasmtime);
    wasm_cmd.arg("run").arg(&module);
    let w = run_io(wasm_cmd, &dir, &dir.join("no.stdin"));

    // Spelled out rather than only compared, because the failure this is about is
    // a plausible number: 21 instead of 42, or a 5 that never became a 7.
    assert_eq!(
        norm(&interp.stdout),
        "7\n14\n8\n42\nlabel\n7\n3\n100\n",
        "the interpreter moved"
    );
    assert_eq!(norm(&interp.stdout), norm(&w.stdout), "stdout");
    assert_eq!(runtime_err(&interp.stderr), runtime_err(&w.stderr), "stderr");
    assert_eq!(interp.status.code(), w.status.code(), "exit");
}

/// The two builtins M2g landed that the corpus does not run, and the boxing bug
/// they found (RFC-0077 M2g).
///
/// `tagged.vyrn` is the only example that reaches `value(x)`, and it reaches it
/// through the `sql"..."` desugar — for `Int64` and `String` only. `charCount` has
/// no example at all: `bytecount.vyrn` stops on a sized-int conversion two lines
/// later, so the lowering would ship untested, which this repo treats as worse
/// than a named gap.
///
/// Both go through the same enum payload word, which is where the bug was: an
/// i32-shaped payload — a `String`, a `Bool` — took ONE scratch local for the
/// value and the box address both, so the box ended up pointing at itself and
/// `print` showed the pointer's bytes. It compiled and it validated. `BoolVal` is
/// here because it is the shape no example builds at all.
#[test]
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test directwasm -- --ignored"]
fn a_boxed_enum_payload_survives_the_word_it_travels_in() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = std::env::temp_dir().join("vyrn-directwasm-value");
    std::fs::create_dir_all(&dir).unwrap();
    let src = "\
fn show(v: Value) -> String {
    return match v {
        IntVal(n) => n.toString(),
        BoolVal(b) => b.toString(),
        StrVal(s) => s,
    }
}

fn main() -> Int64 {
    print(show(value(\"hi there\")))
    print(show(value(true)))
    print(show(value(-7)))
    // Unicode scalar values, not bytes: two of these five are multi-byte.
    print(\"héllo\".charCount())
    print(\"héllo\".byteLength)
    print(\"\".charCount())
    return \"héllo\".charCount()
}
";
    let path = dir.join("valuecount.vyrn");
    std::fs::write(&path, src).unwrap();
    let module = dir.join("valuecount.wasm");
    let build = vyrn()
        .arg("build")
        .arg(&path)
        .arg("--target")
        .arg("wasm")
        .arg("-o")
        .arg(&module)
        .env("VYRN_WASM_BACKEND", "direct")
        .output()
        .expect("build wasm");
    assert!(build.status.success(), "{}", String::from_utf8_lossy(&build.stderr));

    let mut interp_cmd = vyrn();
    interp_cmd.arg("run").arg(&path);
    let interp = run_io(interp_cmd, &dir, &dir.join("no.stdin"));
    let mut wasm_cmd = Command::new(&wasmtime);
    wasm_cmd.arg("run").arg(&module);
    let w = run_io(wasm_cmd, &dir, &dir.join("no.stdin"));

    // Spelled out, because the failure was a plausible-looking string rather than
    // a crash: garbage bytes where "hi there" belonged, and a byte count where a
    // character count belonged.
    assert_eq!(norm(&interp.stdout), "hi there\ntrue\n-7\n5\n6\n0\n", "the interpreter moved");
    assert_eq!(norm(&interp.stdout), norm(&w.stdout), "stdout");
    assert_eq!(runtime_err(&interp.stderr), runtime_err(&w.stderr), "stderr");
    assert_eq!(interp.status.code(), w.status.code(), "exit");
}

/// `bytes` / `slice` / `stringFromBytes`, which no example reaches (RFC-0077 M2g).
///
/// The four examples the ladder filed under `stringFromBytes` all reach it through
/// `std/strings`, and `std/strings` is a wall of five more blockers behind it — so
/// lowering the builtin moved them from its name to `Shr` on `UInt64` and left the
/// lowering itself completely unrun. That is the case this repo treats as worse
/// than a gap, so it gets the running test the corpus cannot supply.
///
/// What is actually being checked is the failure semantics, because they are the
/// part a plausible-looking implementation gets wrong: an embedded NUL is rejected
/// BEFORE the UTF-8 check and with its own wording (a Vyrn `String` is
/// NUL-terminated, so it could not carry one), and the DFA has to reject what
/// Rust's `String::from_utf8` rejects — an overlong form and a lone continuation
/// byte are here for that, not for coverage. `slice`'s two traps are separate
/// programs because a trap ends the run.
#[test]
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test directwasm -- --ignored"]
fn the_string_builtins_agree_with_the_interpreter_about_their_failures() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = std::env::temp_dir().join("vyrn-directwasm-strbytes");
    std::fs::create_dir_all(&dir).unwrap();

    let show = "\
fn show(r: Result<String, String>) -> String {
    return match r {
        Ok(s) => \"ok:\" + s,
        Err(e) => \"err:\" + e,
    }
}
";
    let cases: [(&str, &str); 3] = [
        (
            "ok",
            "\
fn main() -> Int64 {
    print(show(stringFromBytes(bytes(\"héllo\"))))
    print(show(stringFromBytes([]))) // the empty buffer is a valid empty String
    print(show(stringFromBytes(['\\xf0', '\\x9f', '\\x98', '\\x80'])))
    print(slice(\"héllo wörld\", 0, 6))
    print(slice(\"héllo\", 6, 6))     // `to == len` reads the terminator
    print(bytes(\"hé\").length)
    return 0
}
",
        ),
        (
            "bad",
            "\
fn main() -> Int64 {
    print(show(stringFromBytes(['h', '\\x00', 'i'])))  // NUL, not bad UTF-8
    print(show(stringFromBytes(['\\xc0', '\\xaf'])))    // overlong '/'
    print(show(stringFromBytes(['\\x80'])))            // lone continuation
    print(show(stringFromBytes(['\\xed', '\\xa0', '\\x80']))) // a surrogate
    print(show(stringFromBytes(['\\xf5', '\\x80', '\\x80', '\\x80']))) // > U+10FFFF
    print(show(stringFromBytes(['\\xe2', '\\x82'])))    // truncated
    return 0
}
",
        ),
        // Both `slice` traps: out of range, and a cut inside a multi-byte
        // character. The wording is what parity compares, not the fact of trapping.
        (
            "traps",
            "fn main() -> Int64 {\n print(slice(\"hi\", 0, 9))\n return 0\n}\n",
        ),
    ];
    for (what, body) in cases {
        for (name, src) in [
            (what.to_string(), format!("{show}{body}")),
            // The split trap, only for the trapping case: byte 1 of "é" is a
            // continuation byte, so cutting there is the error slicing exists to
            // catch.
            (
                format!("{what}2"),
                "fn main() -> Int64 {\n print(slice(\"hé\", 0, 2))\n return 0\n}\n".to_string(),
            ),
        ] {
            if name.ends_with('2') && what != "traps" {
                continue;
            }
            let path = dir.join(format!("{name}.vyrn"));
            std::fs::write(&path, &src).unwrap();
            let module = dir.join(format!("{name}.wasm"));
            let build = vyrn()
                .arg("build")
                .arg(&path)
                .arg("--target")
                .arg("wasm")
                .arg("-o")
                .arg(&module)
                .env("VYRN_WASM_BACKEND", "direct")
                .output()
                .expect("build wasm");
            assert!(build.status.success(), "{name}: {}", String::from_utf8_lossy(&build.stderr));

            let mut interp_cmd = vyrn();
            interp_cmd.arg("run").arg(&path);
            let interp = run_io(interp_cmd, &dir, &dir.join("no.stdin"));
            let mut wasm_cmd = Command::new(&wasmtime);
            wasm_cmd.arg("run").arg(&module);
            let w = run_io(wasm_cmd, &dir, &dir.join("no.stdin"));

            assert_eq!(norm(&interp.stdout), norm(&w.stdout), "{name}: stdout");
            assert_eq!(runtime_err(&interp.stderr), runtime_err(&w.stderr), "{name}: stderr");
            assert_eq!(interp.status.code(), w.status.code(), "{name}: exit");
            // Comparing two backends would pass if both were silently wrong about
            // which failure happened, so the interpreter's own answer is pinned.
            match what {
                "ok" => assert_eq!(
                    norm(&interp.stdout),
                    "ok:héllo\nok:\nok:😀\nhéllo\n\n3\n",
                    "the interpreter moved"
                ),
                "bad" => assert_eq!(
                    norm(&interp.stdout),
                    "err:bytes contain a NUL byte\n\
                     err:bytes are not valid UTF-8\n\
                     err:bytes are not valid UTF-8\n\
                     err:bytes are not valid UTF-8\n\
                     err:bytes are not valid UTF-8\n\
                     err:bytes are not valid UTF-8\n",
                    "the interpreter moved"
                ),
                _ => assert!(
                    runtime_err(&w.stderr).contains("slice "),
                    "{name}: not a slice trap: {:?}",
                    runtime_err(&w.stderr)
                ),
            }
        }
    }
}

/// Every sized-integer width, and the two answers that are plausible when it is
/// wrong (RFC-0077 M2h).
///
/// `bits.vyrn` reaches the six bitwise operators and `strings.vyrn` reaches
/// `UInt64`, but the example that actually exercises **wrapping at each width** —
/// `sizedints.vyrn` — is blocked on a float conversion, so the ladder cannot see
/// the signed narrow widths at all. Every mistake here compiles, validates and
/// returns a number: wasm has no `i8` arithmetic, so an `Int8` rides an `i32` and
/// a missing renormalization prints 200 where -56 belongs.
///
/// The half worth spelling out is **memory**. `llt` prints `i8` for both `Int8`
/// and `UInt8`, so a load cannot tell from the shape how to extend the byte — a
/// negative `Int8` in a record field or an array element reads back as 197 if it
/// zero-extends, which is a plausible number in a program that never says which
/// it meant. The comparisons are the other silent pair: a signed opcode reads
/// `4000000000` as negative and an unsigned one reads `-59` as enormous, and both
/// answers look like an answer.
///
/// The traps are separate programs because a trap ends the run, and they are here
/// for the widths rather than for the wording: the divide-overflow guard compares
/// against **the width's** minimum, so an `Int8` `-128 / -1` has to trap where a
/// guard written for `Int64` would silently return -128.
#[test]
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test directwasm -- --ignored"]
fn every_integer_width_wraps_where_the_interpreter_wraps() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = std::env::temp_dir().join("vyrn-directwasm-ints");
    std::fs::create_dir_all(&dir).unwrap();

    let widths = "\
type Widths = { a: Int8, b: Int16, c: Int32, d: UInt8, e: UInt32, f: UInt64 }

fn negate(x: Int8) -> Int8 {
    return 0 - x
}

fn main() -> Int64 {
    // Wrapping at each signed width — the operators wasm has to renormalize.
    let a: Int8 = 100
    let b: Int16 = 30000
    let c: Int32 = 2000000000
    print(a * 2)
    print(b + b)
    print(c + c)
    // `0 - x` takes its width from the RIGHT operand, which is the one shape a
    // left-operand-only rule gets wrong.
    print(negate(a))

    // Through memory: a record field and an array element, where a zero-extending
    // load turns -59 into 197.
    let w: Widths = Widths {
        a: Int8(0 - 59),
        b: Int16(0 - 300),
        c: Int32(0 - 7),
        d: 200,
        e: 4000000000,
        f: 18446744073709551615,
    }
    print(w.a)
    print(w.b)
    print(w.c)
    print(w.d)
    print(w.e)
    print(w.f)
    let xs: Array<Int8> = [Int8(0 - 59), 7]
    print(xs[0])
    print(xs[1])

    // Comparisons, where the wrong opcode is a plausible answer both ways.
    print(w.a < 0)
    print(w.e > 2000000000)
    print(w.f > 9223372036854775807)

    // Division and remainder at each signedness.
    print(w.d / 3)
    print(w.d % 3)
    print(w.c / 2)
    print(w.c % 2)
    print(w.f / 3)
    print(w.f % 7)

    // Conversions in both directions, including the two that discard bits.
    print(Int64(w.a))
    print(Int64(w.e))
    print(Int8(w.e))
    print(UInt8(w.a))
    print(Int8(200))
    print(UInt16(w.c))

    // Bitwise at a narrow width: `>>` is arithmetic on the signed one and
    // logical on the unsigned one, and `~` complements inside the width.
    print(w.c & 12)
    print(w.a >> 2)
    print(w.d >> 2)
    print(w.b << 4)
    print(~w.a)
    print(~w.e)
    return 0
}
";
    let path = dir.join("widths.vyrn");
    std::fs::write(&path, widths).unwrap();
    let module = dir.join("widths.wasm");
    let build = vyrn()
        .arg("build")
        .arg(&path)
        .arg("--target")
        .arg("wasm")
        .arg("-o")
        .arg(&module)
        .env("VYRN_WASM_BACKEND", "direct")
        .output()
        .expect("build wasm");
    assert!(build.status.success(), "{}", String::from_utf8_lossy(&build.stderr));

    let mut interp_cmd = vyrn();
    interp_cmd.arg("run").arg(&path);
    let interp = run_io(interp_cmd, &dir, &dir.join("no.stdin"));
    let mut wasm_cmd = Command::new(&wasmtime);
    wasm_cmd.arg("run").arg(&module);
    let w = run_io(wasm_cmd, &dir, &dir.join("no.stdin"));

    // Pinned rather than only compared: two backends can be confidently wrong
    // together about a width, and every number here is one a wrong lowering also
    // produces.
    assert_eq!(
        norm(&interp.stdout),
        "-56\n-5536\n-294967296\n-100\n\
         -59\n-300\n-7\n200\n4000000000\n18446744073709551615\n-59\n7\n\
         true\ntrue\ntrue\n\
         66\n2\n-3\n-1\n6148914691236517205\n1\n\
         -59\n4000000000\n0\n197\n-56\n65529\n\
         8\n-15\n50\n-4800\n58\n294967295\n",
        "the interpreter moved"
    );
    assert_eq!(norm(&interp.stdout), norm(&w.stdout), "stdout");
    assert_eq!(runtime_err(&interp.stderr), runtime_err(&w.stderr), "stderr");
    assert_eq!(interp.status.code(), w.status.code(), "exit");

    // The numeric traps, at widths other than `Int64` — the divide-overflow guard
    // is the one that has to know the width rather than assume 64 bits. Each is a
    // program of its own because a trap ends the run, and the divisor comes out of
    // a call so the checker's const path cannot fold it into a compile error.
    for (what, src, wording) in [
        (
            "minovf",
            "fn neg1() -> Int8 { return Int8(0 - 1) }\n\
             fn main() -> Int64 {\n let m: Int8 = Int8(0 - 128)\n print(m / neg1())\n return 0\n}\n",
            "integer overflow in division",
        ),
        (
            "div0",
            "fn zero() -> UInt8 { return 0 }\n\
             fn main() -> Int64 {\n let x: UInt8 = 200\n print(x / zero())\n return 0\n}\n",
            "division by zero",
        ),
        (
            "rem0",
            "fn zero() -> UInt64 { return 0 }\n\
             fn main() -> Int64 {\n let x: UInt64 = 200\n print(x % zero())\n return 0\n}\n",
            "remainder by zero",
        ),
        // A shift by the width, and by a NEGATIVE amount — one unsigned `>=`
        // covers both, which is the claim being checked rather than asserted.
        (
            "shiftwide",
            "fn eight() -> UInt8 { return 8 }\n\
             fn main() -> Int64 {\n let x: UInt8 = 3\n print(x << eight())\n return 0\n}\n",
            "shift amount out of range",
        ),
        (
            "shiftneg",
            "fn negone() -> Int32 { return Int32(0 - 1) }\n\
             fn main() -> Int64 {\n let x: Int32 = 3\n print(x >> negone())\n return 0\n}\n",
            "shift amount out of range",
        ),
    ] {
        let path = dir.join(format!("{what}.vyrn"));
        std::fs::write(&path, src).unwrap();
        let module = dir.join(format!("{what}.wasm"));
        let build = vyrn()
            .arg("build")
            .arg(&path)
            .arg("--target")
            .arg("wasm")
            .arg("-o")
            .arg(&module)
            .env("VYRN_WASM_BACKEND", "direct")
            .output()
            .expect("build wasm");
        assert!(build.status.success(), "{what}: {}", String::from_utf8_lossy(&build.stderr));

        let mut interp_cmd = vyrn();
        interp_cmd.arg("run").arg(&path);
        let interp = run_io(interp_cmd, &dir, &dir.join("no.stdin"));
        let mut wasm_cmd = Command::new(&wasmtime);
        wasm_cmd.arg("run").arg(&module);
        let w = run_io(wasm_cmd, &dir, &dir.join("no.stdin"));

        assert!(
            runtime_err(&interp.stderr).contains(wording),
            "{what}: the interpreter moved: {:?}",
            runtime_err(&interp.stderr)
        );
        assert_eq!(runtime_err(&interp.stderr), runtime_err(&w.stderr), "{what}: stderr");
        assert_eq!(norm(&interp.stdout), norm(&w.stdout), "{what}: stdout");
        assert_eq!(interp.status.code(), w.status.code(), "{what}: exit");
    }
}

/// The six decimals of a `Float64`, on the values that tell a correct formatter
/// from a plausible one (RFC-0077 M2h).
///
/// `floats.vyrn` prints eleven floats and every one of them is small, finite and
/// ordinary. What `%f` actually is, though, is an EXACT decimal conversion of the
/// double, rounded half-to-EVEN at the sixth place — the interpreter's `{:.6}`
/// and the native build's `printf("%f")` agree on that and nothing that computes
/// six decimals in floating point does. So the cases here are the ones a
/// shortcut gets wrong:
///
/// - `0.0078125` and `0.0234375` are exact ties. Half-to-even keeps the even `2`
///   in the first and rounds the odd `7` up in the second, so a half-UP
///   implementation passes one and fails the other.
/// - `10^300` has 301 integer digits, which is why the numerator is a bignum and
///   not a `u64`. Its digits are not `1` followed by zeros — they are the exact
///   value of the nearest double — so a wrong carry anywhere in the doubling loop
///   shows up as a wrong digit in the middle.
/// - a subnormal reaches `k = 1074`, the deepest the multiply loop goes, and
///   still has to print `0.000000` rather than run off the buffer.
/// - `NaN`, `inf`, `-inf` and `-0.0` are all spelled rather than computed, and
///   `-0.0` keeps a sign that no digit carries.
/// - `Int64` of `10^300` saturates at `Int64.max`, because the interpreter is
///   Rust's `as` and wasm's plain `trunc` would have trapped there.
///
/// Vyrn has no exponent literals, so the extreme values are built by
/// multiplication — which is better than a literal would have been: both engines
/// compute the same double by the same IEEE steps, and the mantissa that comes
/// out is messy rather than round.
#[test]
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test directwasm -- --ignored"]
fn six_decimals_of_a_float_are_the_exact_ones() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = std::env::temp_dir().join("vyrn-directwasm-floats");
    std::fs::create_dir_all(&dir).unwrap();
    let src = "\
fn opaque(x: Float64) -> Float64 {
    return x
}

fn pow10(n: Int64) -> Float64 {
    let mut x: Float64 = 1.0
    let mut i: Int64 = 0
    while i < n {
        x = x * 10.0
        i = i + 1
    }
    return x
}

fn halved(n: Int64) -> Float64 {
    let mut x: Float64 = 1.0
    let mut i: Int64 = 0
    while i < n {
        x = x / 2.0
        i = i + 1
    }
    return x
}

fn main() -> Int64 {
    print(0.0078125)
    print(0.0234375)
    print(pow10(300))
    print(0.0 - pow10(300))
    print(halved(1074))
    print(halved(1075))
    print(opaque(0.0) * (0.0 - 1.0))
    print(0.0 - 0.5)
    print(1.0 / opaque(0.0))
    print(0.0 - 1.0 / opaque(0.0))
    print(opaque(0.0) / opaque(0.0))
    print(9.9999995)
    print(123456789.123456789)
    let f: Float32 = 0.1
    print(f)
    print(f * f)
    print(Int64(0.0 - 2.9))
    print(UInt8(300.7))
    print(Int64(pow10(300)))
    print(Float32(pow10(300)))
    return 0
}
";
    // The exact decimal value of the double nearest 10^300, which is what both
    // references print. Pinned whole because a carry bug in the doubling loop is
    // a wrong digit in the middle rather than at either end.
    const P300: &str = "\
1000000000000000201206451102982726528510718396098215168041874281451248363566\
0941273804370911208852185605358934485189371568149022546577356211033167392772\
7776193144531116603838203491427854077548432800993666474448696900069727411136\
1486849523430568151310289152823685865144042626214886587669241994282008576";
    let path = dir.join("floats6.vyrn");
    std::fs::write(&path, src).unwrap();
    let module = dir.join("floats6.wasm");
    let build = vyrn()
        .arg("build")
        .arg(&path)
        .arg("--target")
        .arg("wasm")
        .arg("-o")
        .arg(&module)
        .env("VYRN_WASM_BACKEND", "direct")
        .output()
        .expect("build wasm");
    assert!(build.status.success(), "{}", String::from_utf8_lossy(&build.stderr));

    let mut interp_cmd = vyrn();
    interp_cmd.arg("run").arg(&path);
    let interp = run_io(interp_cmd, &dir, &dir.join("no.stdin"));
    let mut wasm_cmd = Command::new(&wasmtime);
    wasm_cmd.arg("run").arg(&module);
    let w = run_io(wasm_cmd, &dir, &dir.join("no.stdin"));

    let want = format!(
        "0.007812\n0.023438\n\
         {P300}.000000\n-{P300}.000000\n\
         0.000000\n0.000000\n-0.000000\n-0.500000\n\
         inf\n-inf\nNaN\n\
         9.999999\n123456789.123457\n\
         0.100000\n0.010000\n\
         -2\n44\n9223372036854775807\ninf\n"
    );
    assert_eq!(norm(&interp.stdout), want, "the interpreter moved");
    assert_eq!(norm(&interp.stdout), norm(&w.stdout), "stdout");
    assert_eq!(runtime_err(&interp.stderr), runtime_err(&w.stderr), "stderr");
    assert_eq!(interp.status.code(), w.status.code(), "exit");
}

/// The RFC-0014 semantics the corpus cannot reach, over raw WASI (RFC-0077 M2j).
///
/// Three examples moved to `PASSING` this milestone and between them they exercise
/// the happy path only: `args.vyrn` runs with an EMPTY argv (it has no `.args`
/// fixture, deliberately — the harness gives every example zero arguments),
/// `files.vyrn` reaches exactly one of `readFile`'s three failures, and nothing at
/// all reaches `readLine`, because `input.vyrn` and `vlog.vyrn` are both blocked
/// behind other builtins. So the parts most likely to be plausibly wrong are the
/// parts with no example:
///
/// - **`readLine`'s line rules.** A `\r\n` and a `\n` must read identically or
///   Windows and POSIX pipes disagree; an empty line is `Some("")` and not `None`;
///   a final line with no newline at all is still a line. And `None` is three
///   different things — EOF, a line carrying a NUL byte (which a NUL-terminated
///   `String` could not hold), and a line that is not UTF-8, which is where the
///   interpreter's `String::from_utf8` fails.
/// - **`readFile`'s other two payloads.** The NUL rule fires BEFORE the UTF-8
///   check and has its own wording, so a reader that validated first would report
///   the wrong one — and `readFileBytes` of the same two files must SUCCEED, which
///   is what makes them rules about `String` rather than about reading.
/// - **`args` with an argv.** A token with a space in it is the one that says the
///   pointers are being read out of WASI's own array rather than re-split.
///
/// Everything is pinned against the interpreter's own answer, not just compared
/// between backends: two backends can be confidently wrong together about which
/// failure happened, and every wrong answer here is a plausible-looking one.
#[test]
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test directwasm -- --ignored"]
fn the_wasi_io_builtins_agree_with_the_interpreter_about_their_edges() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = std::env::temp_dir().join("vyrn-directwasm-wasiio");
    std::fs::create_dir_all(&dir).unwrap();
    // Two files a `String` cannot hold, and one it can. Written as bytes, because
    // that is the whole point of them.
    std::fs::write(dir.join("plain.txt"), b"hi\n").unwrap();
    // NOT `nul.bin`: on Windows `nul` is a reserved device name with ANY
    // extension, and wasmtime's capability-based path resolution refuses one — so
    // the file would read as an I/O error under wasm and as five bytes under the
    // interpreter, for a reason that has nothing to do with this backend.
    std::fs::write(dir.join("hasnul.bin"), b"ab\x00cd").unwrap();
    std::fs::write(dir.join("bad.bin"), b"ab\xff\xfecd").unwrap();

    let lines = "\
fn nextLine() -> String {
    return match readLine() {
        Some(s) => \"[\" + s + \"]\",
        None => \"<none>\",
    }
}

fn main() -> Int64 {
    let mut n = 0
    let mut going = true
    while going {
        let s = nextLine()
        if s == \"<none>\" {
            going = false
        } else {
            n = n + 1
            print(\"\\{n} \\{s} \\{s.byteLength}\")
        }
    }
    print(\"lines \\{n}\")
    return n
}
";
    let files = "\
fn show(r: Result<String, String>) -> String {
    return match r {
        Ok(s) => \"ok:\" + s,
        Err(e) => \"err:\" + e,
    }
}

fn size(r: Result<Array<UInt8>, String>) -> Int64 {
    return match r {
        Ok(b) => b.length,
        Err(e) => 0 - 1,
    }
}

fn wrote(r: Result<Bool, String>) -> String {
    return match r {
        Ok(b) => \"ok:\\{b}\",
        Err(e) => \"err:\" + e,
    }
}

fn main() -> Int64 {
    print(show(readFile(\"plain.txt\")))
    print(show(readFile(\"hasnul.bin\")))
    print(show(readFile(\"bad.bin\")))
    print(show(readFile(\"missing.txt\")))
    // The same two files as BYTES: no NUL rule, no UTF-8 rule, so both read.
    print(size(readFileBytes(\"hasnul.bin\")))
    print(size(readFileBytes(\"bad.bin\")))
    print(size(readFileBytes(\"missing.txt\")))
    print(wrote(writeFile(\"nested/nope.txt\", \"x\")))
    print(wrote(writeFile(\"out.tmp.txt\", \"round\")))
    print(show(readFile(\"out.tmp.txt\")))
    return 0
}
";
    let argv = "\
fn main() -> Int64 {
    let a = args()
    print(\"n=\\{a.length}\")
    for x in a {
        print(\"<\\{x}>\")
    }
    return a.length
}
";
    // `\r\n` and `\n` mixed, an empty line, a multi-byte line, then a final line
    // with no terminator at all; the second fixture puts the two unrepresentable
    // lines after a good one, so a reader that stopped early would still print it.
    let stdin_ok: &[u8] = b"alpha\r\nbeta\n\nc\xc3\xa9\nlast, unterminated";
    let stdin_bad: &[u8] = b"good\nwith\x00nul\n";

    let no_args: Vec<String> = Vec::new();
    for (what, src, stdin, prog_args, want) in [
        (
            "lines",
            lines,
            Some(stdin_ok),
            no_args.clone(),
            // The byte length is of the BRACKETED line, so the two constant
            // brackets are in it — which still pins the line, and is what says a
            // `\r` was stripped rather than kept.
            "1 [alpha] 7\n2 [beta] 6\n3 [] 2\n4 [cé] 5\n5 [last, unterminated] 20\nlines 5\n",
        ),
        // The NUL line is `None`, so the loop ends there — one line printed, and
        // the rest of stdin unread. That IS the semantics, not a truncation bug.
        ("nulline", lines, Some(stdin_bad), no_args.clone(), "1 [good] 6\nlines 1\n"),
        (
            "files",
            files,
            None,
            no_args.clone(),
            "ok:hi\n\n\
             err:`hasnul.bin` contains a NUL byte\n\
             err:`bad.bin` is not valid UTF-8\n\
             err:cannot read `missing.txt`\n\
             5\n6\n-1\n\
             err:cannot write `nested/nope.txt`\n\
             ok:true\nok:round\n",
        ),
        (
            "argv",
            argv,
            None,
            vec!["one".into(), "two words".into(), "--three".into()],
            "n=3\n<one>\n<two words>\n<--three>\n",
        ),
    ] {
        let path = dir.join(format!("{what}.vyrn"));
        std::fs::write(&path, src).unwrap();
        let stdin_path = dir.join(format!("{what}.stdin"));
        match stdin {
            Some(bytes) => std::fs::write(&stdin_path, bytes).unwrap(),
            None => {
                let _ = std::fs::remove_file(&stdin_path);
            }
        }
        let module = dir.join(format!("{what}.wasm"));
        let build = vyrn()
            .arg("build")
            .arg(&path)
            .arg("--target")
            .arg("wasm")
            .arg("-o")
            .arg(&module)
            .env("VYRN_WASM_BACKEND", "direct")
            .output()
            .expect("build wasm");
        assert!(build.status.success(), "{what}: {}", String::from_utf8_lossy(&build.stderr));

        let mut interp_cmd = vyrn();
        interp_cmd.arg("run").arg(&path).args(&prog_args);
        let interp = run_io(interp_cmd, &dir, &stdin_path);
        let mut wasm_cmd = Command::new(&wasmtime);
        wasm_cmd.arg("run").arg("--dir").arg(".").arg(&module).args(&prog_args);
        let w = run_io(wasm_cmd, &dir, &stdin_path);

        assert_eq!(norm(&interp.stdout), want, "{what}: the interpreter moved");
        assert_eq!(norm(&interp.stdout), norm(&w.stdout), "{what}: stdout");
        assert_eq!(runtime_err(&interp.stderr), runtime_err(&w.stderr), "{what}: stderr");
        assert_eq!(interp.status.code(), w.status.code(), "{what}: exit");
    }
}

/// `?` reaches the epilogue, proved by the two things skipping it would break
/// (RFC-0077 M2k).
///
/// M1's rule is that a body must not emit `return`: the shadow-stack release, and
/// since M2f the `modify` copy-back, sit after the block every exit branches to.
/// `?` is an early exit, so it is exactly the construct that can get this wrong,
/// and both failures are invisible in a small program.
///
/// So: 20,000 calls that each propagate. A frame that is claimed and not released
/// walks the stack pointer down past 0, where it wraps to `0xFFFFFFF8` and the
/// next slot access is out of bounds — checked by ACTUALLY EMITTING
/// `Instruction::Return` here once, which traps `out of bounds memory access`
/// before the first `print`. And `s.n` counts the writes made BEFORE each
/// propagation: 20,000 means every propagating call copied its `modify` parameter
/// back, 0 would mean none did.
#[test]
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test directwasm -- --ignored"]
fn a_propagating_early_exit_releases_its_frame_and_copies_modify_back() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = std::env::temp_dir().join("vyrn-directwasm-try");
    std::fs::create_dir_all(&dir).unwrap();
    let src = "\
type Sink = { n: Int64 }

fn bump(s: modify Sink, o: Option<Int64>) -> Option<Int64> {
    s.n = s.n + 1
    let v = o?
    s.n = s.n + 100
    return Some(v)
}

fn main() -> Int64 {
    let mut s = Sink { n: 0 }
    let mut i = 0
    while i < 20000 {
        bump(s, None)
        i = i + 1
    }
    print(s.n)
    bump(s, Some(7))
    print(s.n)
    return 0
}
";
    let path = dir.join("try.vyrn");
    std::fs::write(&path, src).unwrap();
    let module = dir.join("try.wasm");
    let build = vyrn()
        .arg("build")
        .arg(&path)
        .arg("--target")
        .arg("wasm")
        .arg("-o")
        .arg(&module)
        .env("VYRN_WASM_BACKEND", "direct")
        .output()
        .expect("build wasm");
    assert!(build.status.success(), "{}", String::from_utf8_lossy(&build.stderr));

    let mut interp_cmd = vyrn();
    interp_cmd.arg("run").arg(&path);
    let interp = run_io(interp_cmd, &dir, &dir.join("no.stdin"));
    let mut wasm_cmd = Command::new(&wasmtime);
    wasm_cmd.arg("run").arg(&module);
    let w = run_io(wasm_cmd, &dir, &dir.join("no.stdin"));

    assert_eq!(norm(&interp.stdout), "20000\n20101\n", "the interpreter moved");
    assert_eq!(norm(&interp.stdout), norm(&w.stdout), "stdout");
    assert_eq!(runtime_err(&interp.stderr), runtime_err(&w.stderr), "stderr");
    assert_eq!(interp.status.code(), w.status.code(), "exit");
}

/// `std/jsonread` through the direct backend (RFC-0077 M2k) — the claim that
/// matters more than the two examples the milestone added.
///
/// The reader is the module RFC-0078 M3's `fromJson` is built on, and it was
/// unbuildable here for exactly two reasons: `?` (six sites) and `if let`. So this
/// is the thing that says M3 can land on all three engines at once rather than on
/// the interpreter and the native build while wasm waits — and it says it by
/// PARSING, not by compiling: a `?` that copied the wrong width, took the wrong
/// `br`, or skipped the payload decode builds fine and gets a different answer.
///
/// The inputs are chosen for the parser's own control flow rather than for JSON
/// coverage: a nested document (recursion through `?`, aggregates returned through
/// the hidden destination), a surrogate pair (`readHex4`'s `?` twice in one
/// expression, the only nested one in the module), and four rejections whose
/// `line N, col M:` wording is an `Err` propagated out through six frames.
#[test]
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test directwasm -- --ignored"]
fn the_json_reader_parses_the_same_on_the_direct_backend() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = std::env::temp_dir().join("vyrn-directwasm-jsonread");
    std::fs::create_dir_all(&dir).unwrap();
    let src = "\
import { parseJson } from \"std/jsonread\"
import { Json, emit } from \"std/json\"

fn show(src: String) -> Int64 {
    print(match parseJson(src) {
        Ok(v) => emit(v),
        Err(e) => \"err: \\{e}\",
    })
    return 0
}

fn main() -> Int64 {
    show(\"{\\\"a\\\": [1, 2, {\\\"b\\\": null}], \\\"c\\\": \\\"hi\\\\u00e9\\\"}\")
    show(\"  true \")
    show(\"[1, 2,]\")
    show(\"{\\\"k\\\": 1, \\\"k\\\": 2}\")
    show(\"\\\"\\\\ud83d\\\\ude00\\\"\")
    show(\"[1, 2\")
    show(\"-0.5e+10\")
    if let Ok(v) = parseJson(\"[]\") {
        print(\"empty: \\{emit(v)}\")
    }
    return 0
}
";
    let path = dir.join("jsonread.vyrn");
    std::fs::write(&path, src).unwrap();
    let module = dir.join("jsonread.wasm");
    let build = vyrn()
        .arg("build")
        .arg(&path)
        .arg("--target")
        .arg("wasm")
        .arg("-o")
        .arg(&module)
        .env("VYRN_WASM_BACKEND", "direct")
        .output()
        .expect("build wasm");
    assert!(build.status.success(), "{}", String::from_utf8_lossy(&build.stderr));

    let mut interp_cmd = vyrn();
    interp_cmd.arg("run").arg(&path);
    let interp = run_io(interp_cmd, &dir, &dir.join("no.stdin"));
    let mut wasm_cmd = Command::new(&wasmtime);
    wasm_cmd.arg("run").arg(&module);
    let w = run_io(wasm_cmd, &dir, &dir.join("no.stdin"));

    // Pinned, so a lowering that made both engines agree on nothing useful — an
    // `Err` for every input, say — is still red.
    assert_eq!(
        norm(&interp.stdout),
        "{\"a\":[1,2,{\"b\":null}],\"c\":\"hi\u{e9}\"}\n\
         true\n\
         err: line 1, col 7: trailing comma before ']'\n\
         err: line 1, col 13: duplicate object key: k\n\
         \"\u{1f600}\"\n\
         err: line 1, col 6: unterminated array\n\
         -0.5e+10\n\
         empty: []\n",
        "the reader moved"
    );
    assert_eq!(norm(&interp.stdout), norm(&w.stdout), "stdout");
    assert_eq!(runtime_err(&interp.stderr), runtime_err(&w.stderr), "stderr");
    assert_eq!(interp.status.code(), w.status.code(), "exit");
}

/// The construct a build failed on, with the source line dropped so the same gap
/// at fifty sites is one entry. Anything that is not a lowering gap is reported
/// whole — an ICE or a load error is not a burndown item.
fn blocker(stderr: &str) -> String {
    for line in stderr.lines() {
        if let Some(rest) = line.trim().strip_prefix("error: direct backend: no lowering for ") {
            return rest.rsplit_once(" at line ").map(|(what, _)| what).unwrap_or(rest).to_string();
        }
    }
    stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("(no message)").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::blocker;

    #[test]
    fn the_grouping_key_is_the_construct_not_the_site() {
        assert_eq!(
            blocker("error: direct backend: no lowering for `while` at line 12"),
            "`while`"
        );
        assert_eq!(
            blocker("error: direct backend: no lowering for `while` at line 99"),
            "`while`"
        );
        // Not a lowering gap: reported whole rather than mined for a construct.
        assert_eq!(blocker("error: cannot read foo.vyrn"), "error: cannot read foo.vyrn");
    }
}
