//! One source builds to one artifact, every time, in every process.
//!
//! A build nobody can repeat is a build nobody can check; `vyrn.lock` pins
//! remote modules by sha256 for the same reason. The guarded defect is a
//! per-process seeded container (`HashSet`, `HashMap`) whose iteration order
//! reaches the output: iterating module-state accumulators to reserve ownership
//! words moved every later static address.
//!
//! Each row spawns the compiler afresh, because a `HashSet` iterates the same
//! way twice inside one process. The rows compare bytes, not containers: only
//! the artifact can say whether a container decides the output.

use std::path::PathBuf;
use std::process::Command;

mod common;

/// How many separate compilers have to agree. Three accumulators have six orders,
/// so seven runs agree by luck about once in 300,000. A deterministic compiler
/// always passes.
const RUNS: usize = 7;

/// Several module-state `String` accumulators, plus a generic and a stored `fn`
/// value, so an unordered worklist or registry is caught too.
///
/// The accumulators grow by `g = g + ...` and are read only where the pointer
/// cannot be retained (an interpolation copies). Otherwise
/// `global_append_candidates` bans them, no ownership word is reserved, and the
/// test passes with the defect live.
const SRC: &str = r#"let mut alpha: String = ""
let mut beta: String = ""
let mut gamma: String = ""

fn twin<T>(x: T) -> Array<T> {
    return [x.copy(), x.copy()]
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

/// Writes `SRC` in a scratch directory; the guard owns it and the path names the file.
fn source() -> (common::Scratch, PathBuf) {
    let dir = common::scratch("reproducible");
    let f = dir.join("repro.vyrn");
    std::fs::write(&f, SRC).unwrap();
    (dir, f)
}

fn vyrn() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vyrn"))
}

/// Panics with the run that disagreed and the first differing byte.
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

/// A grammar writes the same parser in every process: the writer's tables are ordered
/// by the file, and its rows by name.
#[test]
fn the_same_grammar_writes_the_same_parser_in_every_process() {
    let example =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/syntaxcalc.vyrn");
    let runs: Vec<Vec<u8>> = (0..RUNS)
        .map(|_| {
            let out = vyrn()
                .arg("emit-gen")
                .arg(&example)
                .output()
                .expect("vyrn emit-gen");
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            out.stdout
        })
        .collect();
    all_equal(&runs, "the written parser");
}

#[test]
fn the_same_source_builds_to_the_same_wasm_bytes_in_every_process() {
    let (dir, src) = source();
    let outs: Vec<Vec<u8>> = (0..RUNS)
        .map(|i| {
            let out = dir.join(format!("repro{i}.wasm"));
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

/// The lowering keeps two `HashMap`s (the checker's per-node answers and the
/// per-instance substitution); neither may reach the dump's order.
#[test]
fn the_same_source_lowers_to_the_same_text_in_every_process() {
    let (_dir, src) = source();
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

/// A key built from a node's address is sound only while the node lives (#444).
/// Compiling a second program and dropping it frees addresses the first program
/// reuses when compiled again, so a key that outlived its node shows as
/// different bytes.
#[test]
fn a_program_compiled_after_another_in_one_process_is_the_same_bytes() {
    struct Disk;
    impl vyrn_frontend::loader::ModuleResolver for Disk {
        fn read(&self, resolved: &str) -> Result<String, String> {
            std::fs::read_to_string(resolved).map_err(|e| e.to_string())
        }
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    // The `vyrn run` sequence.
    let build = |name: &str| -> Vec<u8> {
        let path = root.join("examples").join(name);
        let src = std::fs::read_to_string(&path).expect("the example reads");
        let key = path.to_string_lossy().replace('\\', "/");
        let opts = vyrn_frontend::loader::LoadOptions {
            std_root: Some(root.join("std").to_string_lossy().replace('\\', "/")),
            expansions: vyrn_frontend::project::Expansions::shared(),
            ..Default::default()
        };
        let engine = vyrn_genwasm::engine();
        let (program, world) = vyrn_lower::load_warned(&src, &key, &opts, &Disk, Some(&*engine))
            .0
            .expect("the example loads");
        vyrn_codegen::check_instantiations(&world).expect("the example instantiates");
        vyrn_codegen::direct::compile(&program, world).expect("the example compiles")
    };
    let first = build("fnvalarg.vyrn");
    let _other = build("closures2.vyrn");
    let again = build("fnvalarg.vyrn");
    all_equal(
        &[first, again],
        "fnvalarg.vyrn compiled before and after closures2.vyrn",
    );
}

/// Sixty functions that index arrays, so typing, placing, the summaries and the
/// emitter each weigh above `par`'s serial cut. `edited` changes one body and adds
/// an import, as an editor keystroke does.
fn many(edited: bool) -> String {
    let mut src = String::new();
    if edited {
        src.push_str("import { contains } from \"std/strpred\"\n\n");
    }
    for i in 0..60 {
        let step = if edited && i == 3 { i + 1 } else { i };
        src.push_str(&format!(
            "fn f{i}(xs: Array<Int64>, k: Int64) -> Int64 {{\n    let mut s = k\n    \
             let mut j = 0\n    while j < xs.length {{\n        s = s + xs[j] * {step}\n        \
             j = j + 1\n    }}\n    return s\n}}\n\n"
        ));
    }
    src.push_str("fn main() -> Int64 {\n    let xs: Array<Int64> = [1, 2, 3]\n    let mut t = 0\n");
    for i in 0..60 {
        src.push_str(&format!("    t = t + f{i}(xs, {i})\n"));
    }
    if edited {
        src.push_str("    if contains(\"abc\", \"b\") {\n        t = t + 1\n    }\n");
    }
    src.push_str("    print(\"\\{t}\")\n    return 0\n}\n");
    src
}

/// A helper thread of `par::in_parallel` outlives the call that used it, so a
/// compile must find nothing an earlier one left there: in one process, A, then
/// an edited B, then A again build the bytes a fresh `vyrn build` builds for
/// each, and a compile that records nothing after one that did records nothing.
#[test]
fn a_helper_thread_carries_nothing_from_one_compile_to_the_next() {
    struct Disk;
    impl vyrn_frontend::loader::ModuleResolver for Disk {
        fn read(&self, resolved: &str) -> Result<String, String> {
            std::fs::read_to_string(resolved).map_err(|e| e.to_string())
        }
    }
    assert!(
        std::thread::available_parallelism().map_or(1, |n| n.get()) > 1
            && std::env::var("VYRN_THREADS").map_or(true, |n| n != "1"),
        "the test needs more than one thread"
    );
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = common::scratch("helpers");
    let (a, b) = (dir.join("a.vyrn"), dir.join("b.vyrn"));
    std::fs::write(&a, many(false)).unwrap();
    std::fs::write(&b, many(true)).unwrap();
    let build = |path: &PathBuf| -> Vec<u8> {
        let src = std::fs::read_to_string(path).expect("the root reads");
        let key = path.to_string_lossy().replace('\\', "/");
        let opts = vyrn_frontend::loader::LoadOptions {
            std_root: Some(root.join("std").to_string_lossy().replace('\\', "/")),
            expansions: vyrn_frontend::project::Expansions::shared(),
            ..Default::default()
        };
        let engine = vyrn_genwasm::engine();
        let (program, world) = vyrn_lower::load_warned(&src, &key, &opts, &Disk, Some(&*engine))
            .0
            .expect("the root loads");
        vyrn_codegen::check_instantiations(&world).expect("the root instantiates");
        vyrn_codegen::direct::compile(&program, world).expect("the root compiles")
    };
    let fresh = |path: &PathBuf| -> Vec<u8> {
        let out = dir.join("fresh.wasm");
        let o = vyrn()
            .current_dir(&root)
            .args(["build", &path.to_string_lossy(), "--target", "wasm", "-o"])
            .arg(&out)
            .output()
            .expect("run vyrn");
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        std::fs::read(&out).expect("the build writes")
    };
    let observed = |path: &PathBuf| {
        vyrn_codegen::observe::start();
        let wasm = build(path);
        vyrn_codegen::observe::stop();
        (wasm, vyrn_codegen::observe::take_typings().len())
    };
    let (first, typed) = observed(&a);
    assert!(typed > 0, "recording on, the emitter records its typings");
    let edited = build(&b);
    let unrecorded = vyrn_codegen::observe::take_typings().len();
    let (again, typed_again) = observed(&a);
    assert_eq!(
        unrecorded, 0,
        "a compile with recording off recorded typings"
    );
    assert_eq!(typed_again, typed, "A recorded another count of typings");
    let (fresh_a, fresh_b) = (fresh(&a), fresh(&b));
    all_equal(
        &[fresh_a.clone(), first],
        "A first in the process and fresh",
    );
    all_equal(&[fresh_b, edited], "B after A in the process and fresh");
    all_equal(&[fresh_a, again], "A after B in the process and fresh");
}
