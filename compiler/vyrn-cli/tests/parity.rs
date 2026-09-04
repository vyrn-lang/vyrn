//! Corpus parity harness: every example must behave byte-identically under the
//! interpreter (`vyrn run`, the reference semantics), the native binary
//! (`vyrn build` + execute) and wasm (`vyrn build --target wasm` under
//! `wasmtime`). Compares stdout, stderr, and exit code.
//!
//! Ignored by default (needs `clang` for the native column and builds every
//! example — ~a minute):
//!
//!     cargo test -p vyrn-cli --test parity -- --ignored --nocapture
//!
//! Line endings are normalized (CRLF → LF): the interpreter writes LF while
//! the native binary inherits the platform's text-mode CRLF — a documented,
//! benign difference.
//!
//! # One gate, since RFC-0077 M5
//!
//! The wasm column was produced by clang until M5 and is now the direct backend,
//! which is why this file also holds the direct-backend cases the corpus does not
//! reach. Those lived in a `directwasm` tier for the length of M2, beside a
//! `PASSING` burndown list that was the milestone's real deliverable; the list
//! reached 87 of 87 and the tier then measured exactly what the column below
//! measures. Two gates over one corpus is how a number stops being about the
//! thing it names — and the tier was never in CI, which ran only this file, so
//! folding it in is also the first time those cases are gated at all.
//!
//! The pins are the other half. Every one of them exists because no example
//! reaches the path: a bounds message whose wrong wording reads exactly like a
//! check that never fires, a DFA walk over a non-ASCII byte, a column off both
//! ends of a buffer, a suppressed log call, a
//! renumbering after the sweep. Each compares against the INTERPRETER's own
//! answer rather than against a spelling written here, because two backends can be
//! confidently wrong together.

mod common;
use common::*;
use std::path::{Path, PathBuf};
use std::process::Command;

#[test]
#[ignore = "needs clang; run explicitly: cargo test -p vyrn-cli --test parity -- --ignored"]
fn examples_interp_native_parity() {
    let dir = examples_dir();
    let out_dir = scratch("parity-corpus");
    // A `wasmtime` binary and nothing else: since RFC-0077 M5 the wasm column is
    // emitted directly, so no clang, no wasi sysroot and no builtins archive stand
    // between this harness and a module.
    let wasm = wasmtime();
    if wasm.is_none() {
        eprintln!("NOTE: no wasmtime — verifying interp == native only");
    }

    let mut names: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "vyrn"))
        .collect();
    names.sort();
    assert!(!names.is_empty(), "no examples found in {}", dir.display());

    let mut failures: Vec<String> = Vec::new();
    // Three reasons an example is not compared, counted apart, because the summary
    // line used to call all of them "known divergent" while that list was empty.
    // A refusal is a program `vyrn check` rejects on purpose, asserted by
    // `expected_check_failures_do_fail`; host-only is `extern` into a namespace
    // only a browser supplies, asserted by `wasm_only_examples_trap_identically`.
    let (mut divergent, mut refused, mut host_only) = (0usize, 0usize, 0usize);

    for path in &names {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if let Some((_, why)) = KNOWN_DIVERGENT.iter().find(|(n, _)| *n == name) {
            eprintln!("SKIP  {name}  ({why})");
            divergent += 1;
            continue;
        }
        if let Some((_, why, _)) = EXPECTED_CHECK_FAILURE.iter().find(|(n, ..)| *n == name) {
            eprintln!("SKIP  {name}  (expected check failure: {why})");
            refused += 1;
            continue;
        }
        if let Some((_, why)) = WASM_ONLY.iter().find(|(n, _)| *n == name) {
            eprintln!("SKIP  {name}  (wasm-only: {why})");
            host_only += 1;
            continue;
        }
        if let Some((_, why)) = NATIVE_UNSUPPORTED.iter().find(|(n, _)| *n == name) {
            eprintln!("SKIP  {name}  (no native lowering: {why})");
            host_only += 1;
            continue;
        }

        // RFC-0014 conventions: `examples/<name>.stdin` pipes into all three
        // backends; every run's cwd is `examples/` so relative file paths in
        // the example resolve identically everywhere.
        let stdin_fixture = path.with_extension("stdin");
        // RFC-0061: `examples/<name>.args` forwards program arguments (argv[1..])
        // to all three backends byte-identically. No fixture ⇒ empty argv.
        let prog_args = read_args(&path.with_extension("args"));

        let mut interp_cmd = vyrn();
        interp_cmd.arg("run").arg(path);
        interp_cmd.args(&prog_args);
        let interp = run_io(interp_cmd, &dir, &stdin_fixture);

        let exe = out_dir.join(format!("{name}.exe"));
        let build = vyrn()
            .arg("build")
            .arg(path)
            .arg("-o")
            .arg(&exe)
            .output()
            .expect("build");
        if !build.status.success() {
            failures.push(format!(
                "{name}: native build failed:\n{}{}",
                norm(&build.stdout),
                norm(&build.stderr)
            ));
            continue;
        }
        let mut native_cmd = Command::new(&exe);
        native_cmd.env("VYRN_FREE_AUDIT", "1");
        native_cmd.args(&prog_args);
        let native = run_io(native_cmd, &dir, &stdin_fixture);

        let (i_out, n_out) = (norm(&interp.stdout), norm(&native.stdout));
        let (i_err, n_err) = (runtime_err(&interp.stderr), runtime_err(&native.stderr));
        let (i_code, n_code) = (interp.status.code(), native.status.code());

        if i_out != n_out || i_err != n_err || i_code != n_code {
            failures.push(format!(
                "{name}: DIVERGED\n  exit: interp {i_code:?} vs native {n_code:?}\n{}{}",
                first_diff("stdout", "interp", &i_out, "native", &n_out).unwrap_or_default(),
                first_diff("stderr", "interp", &i_err, "native", &n_err).unwrap_or_default(),
            ));
            continue;
        }

        // Third column: the same program compiled to wasm32-wasi must match
        // the interpreter byte-for-byte too (wasm writes LF like the interp;
        // norm() makes it moot either way).
        if let Some(wasmtime) = &wasm {
            let module = out_dir.join(format!("{name}.wasm"));
            let build = vyrn()
                .arg("build")
                .arg(path)
                .arg("--target")
                .arg("wasm")
                .arg("-o")
                .arg(&module)
                .output()
                .expect("build wasm");
            if !build.status.success() {
                failures.push(format!(
                    "{name}: wasm build failed:\n{}{}",
                    norm(&build.stdout),
                    norm(&build.stderr)
                ));
                continue;
            }
            // `--dir .` preopens the (already-set) working directory —
            // `examples/` — so WASI file access sees the same tree the other
            // two backends do (wasmtime v46: `--dir <HOST_DIR[::GUEST_DIR]>`).
            let mut wasm_cmd = Command::new(wasmtime);
            wasm_cmd.arg("run").arg("--dir").arg(".");
            // Forward the RFC-0043 fixed clock/seed into the guest: wasmtime does
            // not inherit host env, so the shim's getenv only sees them via --env.
            wasm_cmd
                .arg("--env")
                .arg(format!("VYRN_FIXED_TIME={FIXED_TIME}"));
            wasm_cmd
                .arg("--env")
                .arg(format!("VYRN_FIXED_SEED={FIXED_SEED}"));
            wasm_cmd.arg(&module);
            // wasmtime forwards guest argv AFTER the module path (RFC-0061).
            wasm_cmd.args(&prog_args);
            let w = run_io(wasm_cmd, &dir, &stdin_fixture);
            let (w_out, w_err) = (norm(&w.stdout), runtime_err(&w.stderr));
            let w_code = w.status.code();
            if i_out != w_out || i_err != w_err || i_code != w_code {
                failures.push(format!(
                    "{name}: WASM DIVERGED\n  exit: interp {i_code:?} vs wasm {w_code:?}\n{}{}",
                    first_diff("stdout", "interp", &i_out, "wasm", &w_out).unwrap_or_default(),
                    first_diff("stderr", "interp", &i_err, "wasm", &w_err).unwrap_or_default(),
                ));
                continue;
            }
        }
        eprintln!("ok    {name}");
    }

    let skipped = divergent + refused + host_only;
    eprintln!(
        "\nparity: {} checked, {} skipped ({} refused by `vyrn check`, {} host-only or native-unsupported, {} known divergent), {} failed",
        names.len() - skipped,
        skipped,
        refused,
        host_only,
        divergent,
        failures.len()
    );
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

/// What a red run says, which is the one part of this file a green run never
/// prints — so it is asserted here rather than discovered on the day it matters.
///
/// Not `#[ignore]`d: it compiles nothing and runs no engine.
#[test]
fn a_divergence_names_the_first_differing_line() {
    let interp = "start\nsame\nsame\n42\ntail\n";
    let native = "start\nsame\nsame\n43\ntail\n";
    let msg = first_diff("stdout", "interp", interp, "native", native).expect("they differ");
    assert!(
        msg.contains("stdout: first differs at line 4 (interp 5 lines, native 5 lines)"),
        "{msg}"
    );
    assert!(msg.contains("same     3 | same"), "shared context:\n{msg}");
    assert!(msg.contains("interp     4 | 42"), "{msg}");
    assert!(msg.contains("native     4 | 43"), "{msg}");
    assert!(
        !msg.contains("start"),
        "a whole transcript is what this replaced:\n{msg}"
    );

    // Identical bytes is the only thing that passes, and a difference the line
    // view cannot see — here a missing trailing newline — is not laundered into
    // one: `lines()` reports the same two lines for both.
    assert!(first_diff("stdout", "interp", interp, "native", interp).is_none());
    let msg = first_diff("stdout", "interp", "a\nb\n", "wasm", "a\nb").expect("bytes differ");
    assert!(
        msg.contains("the 2 lines are equal, the bytes are not — first differs at byte 3"),
        "{msg}"
    );

    // A line the other engine does not have at all.
    let msg = first_diff("stderr", "interp", "a\nb\n", "wasm", "a\n").expect("they differ");
    assert!(msg.contains("first differs at line 2"), "{msg}");
    assert!(msg.contains("wasm     2 | <no such line>"), "{msg}");
}

/// The intentional-compile-error examples must actually fail `vyrn check` (and
/// name a validation diagnostic) — a guard so a silently-fixed example doesn't
/// keep claiming to demonstrate a rejection. Runs without clang, so it is not
/// `#[ignore]`d.
/// The wasm-only (extern-calling) examples must trap with the canonical
/// wording on BOTH non-wasm targets, byte-identically — the RFC-0012 parity
/// rule. Needs clang for the native half, so it is `#[ignore]`d like the main
/// parity run.
#[test]
#[ignore = "needs clang; run explicitly: cargo test -p vyrn-cli --test parity -- --ignored"]
fn wasm_only_examples_trap_identically() {
    let dir = examples_dir();
    let out_dir = scratch("parity-wasmonly");
    for (name, _why) in WASM_ONLY {
        let path = dir.join(name);

        let interp = vyrn().arg("run").arg(&path).output().expect("run interp");
        assert_eq!(
            interp.status.code(),
            Some(1),
            "{name}: interp must trap (exit 1)"
        );
        let i_err = norm(&interp.stderr);
        assert!(
            i_err.contains("is not available on this target"),
            "{name}: interp must print the canonical extern trap, got:\n{i_err}"
        );

        let exe = out_dir.join(format!("{name}.exe"));
        let build = vyrn()
            .arg("build")
            .arg(&path)
            .arg("-o")
            .arg(&exe)
            .output()
            .expect("build");
        assert!(
            build.status.success(),
            "{name}: native build must succeed (extern trap stubs link):\n{}",
            norm(&build.stderr)
        );
        let native = Command::new(&exe)
            .env("VYRN_FREE_AUDIT", "1")
            .output()
            .expect("run native");
        assert_eq!(
            native.status.code(),
            Some(1),
            "{name}: native must trap (exit 1)"
        );
        assert_eq!(
            norm(&native.stderr),
            i_err,
            "{name}: interp and native extern traps must be byte-identical"
        );
        assert_eq!(
            norm(&native.stdout),
            norm(&interp.stdout),
            "{name}: stdout identical too"
        );
    }
}

/// Worker threads (RFC-0025): with `spawn` on real OS threads natively,
/// (a) the threaded run, the `VYRN_SEQUENTIAL_SPAWN=1` eager run, and the
/// interpreter all produce byte-identical output on the spawn-heavy example,
/// and (b) a trap INSIDE a task keeps the locked protocol — the canonical
/// wording printed exactly once on stderr, exit 1 — in all three modes.
#[test]
#[ignore = "needs clang; run explicitly: cargo test -p vyrn-cli --test parity -- --ignored"]
fn threaded_spawn_matches_sequential_and_interp() {
    let dir = examples_dir();
    let out_dir = scratch("parity-spawn");
    let path = dir.join("parallel.vyrn");

    let interp = vyrn().arg("run").arg(&path).output().expect("run interp");
    let exe = out_dir.join("parallel-seq-check.exe");
    let build = vyrn()
        .arg("build")
        .arg(&path)
        .arg("-o")
        .arg(&exe)
        .output()
        .expect("build");
    assert!(
        build.status.success(),
        "native build failed:\n{}",
        norm(&build.stderr)
    );

    let threaded = Command::new(&exe)
        .env("VYRN_FREE_AUDIT", "1")
        .output()
        .expect("run threaded");
    let sequential = Command::new(&exe)
        .env("VYRN_FREE_AUDIT", "1")
        .env("VYRN_SEQUENTIAL_SPAWN", "1")
        .output()
        .expect("run sequential");

    for (label, run) in [
        ("threaded", &threaded),
        ("VYRN_SEQUENTIAL_SPAWN=1", &sequential),
    ] {
        assert_eq!(
            norm(&run.stdout),
            norm(&interp.stdout),
            "{label}: stdout != interp"
        );
        assert_eq!(
            norm(&run.stderr),
            norm(&interp.stderr),
            "{label}: stderr != interp"
        );
        assert_eq!(
            run.status.code(),
            interp.status.code(),
            "{label}: exit code != interp"
        );
    }
}

#[test]
#[ignore = "needs clang; run explicitly: cargo test -p vyrn-cli --test parity -- --ignored"]
fn task_trap_prints_once_and_exits_1_threaded() {
    let out_dir = scratch("parity-tasktrap");
    // A long task in flight while a second task traps: the trapping task
    // performs the standard trap protocol itself (stderr + exit(1)) from its
    // own thread — the locked RFC-0025 semantics.
    //
    // `w` is DROPPED rather than joined since RFC-0095 M1: a task is linear, so
    // a program that walks away from one no longer compiles. The drop is after
    // the join on purpose — the trap normally ends the process first, and the
    // shape being pinned is a live task in flight when another one traps.
    let src = "fn boom(n: Int64) -> Int64 {\n    let z = n - n\n    return n / z\n}\n\n\
               fn fib(n: Int64) -> Int64 {\n    if n < 2 { return n }\n    \
               return fib(n - 1) + fib(n - 2)\n}\n\n\
               fn main() -> Int64 {\n    let w = spawn fib(30)\n    \
               let t = spawn boom(3)\n    let r = t.join()\n    drop w\n    return r\n}\n";
    let file = out_dir.join("taskboom.vyrn");
    std::fs::write(&file, src).unwrap();
    let exe = out_dir.join("taskboom.exe");
    let build = vyrn()
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .output()
        .expect("build");
    assert!(
        build.status.success(),
        "native build failed:\n{}",
        norm(&build.stderr)
    );

    let interp = vyrn().arg("run").arg(&file).output().expect("run interp");
    let threaded = Command::new(&exe)
        .env("VYRN_FREE_AUDIT", "1")
        .output()
        .expect("run threaded");
    let sequential = Command::new(&exe)
        .env("VYRN_FREE_AUDIT", "1")
        .env("VYRN_SEQUENTIAL_SPAWN", "1")
        .output()
        .expect("run sequential");

    for (label, run) in [
        ("interp", &interp),
        ("threaded", &threaded),
        ("VYRN_SEQUENTIAL_SPAWN=1", &sequential),
    ] {
        assert_eq!(run.status.code(), Some(1), "{label}: task trap must exit 1");
        assert_eq!(norm(&run.stdout), "", "{label}: no stdout");
        assert_eq!(
            norm(&run.stderr),
            "error: division by zero\n",
            "{label}: canonical wording, printed exactly once"
        );
    }
}

/// RFC-0095 M1: a task that is **dropped** rather than joined keeps the trap
/// protocol.
///
/// This is the milestone's own risk, written as a test. `drop t` gives the
/// task's storage back, and the RFC says plainly that a drop which skipped the
/// WAIT would swallow the trap and one which freed the frame early would corrupt
/// the heap. So the drop waits first, and the trapping task then prints the
/// canonical line once and exits 1 — from its own thread, from the main thread
/// under `VYRN_SEQUENTIAL_SPAWN=1`, and from the interpreter's eager call.
///
/// The program prints BEFORE the drop, and that line must survive: `exit()`
/// flushes stdout, so the trap is not allowed to lose it.
#[test]
#[ignore = "needs clang; run explicitly: cargo test -p vyrn-cli --test parity -- --ignored"]
fn a_dropped_task_that_traps_still_prints_once_and_exits_1() {
    let out_dir = scratch("parity-taskdrop");
    let src = "fn boom(n: Int64) -> Int64 {\n    let z = n - n\n    return n / z\n}\n\n\
               fn main() -> Int64 {\n    print(\"before\")\n    \
               let t = spawn boom(3)\n    drop t\n    return 0\n}\n";
    let file = out_dir.join("taskdropboom.vyrn");
    std::fs::write(&file, src).unwrap();
    let exe = out_dir.join("taskdropboom.exe");
    let build = vyrn()
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .output()
        .expect("build");
    assert!(
        build.status.success(),
        "native build failed:\n{}",
        norm(&build.stderr)
    );

    let interp = vyrn().arg("run").arg(&file).output().expect("run interp");
    let threaded = Command::new(&exe)
        .env("VYRN_FREE_AUDIT", "1")
        .output()
        .expect("run threaded");
    let sequential = Command::new(&exe)
        .env("VYRN_FREE_AUDIT", "1")
        .env("VYRN_SEQUENTIAL_SPAWN", "1")
        .output()
        .expect("run sequential");

    for (label, run) in [
        ("interp", &interp),
        ("threaded", &threaded),
        ("VYRN_SEQUENTIAL_SPAWN=1", &sequential),
    ] {
        assert_eq!(
            run.status.code(),
            Some(1),
            "{label}: a dropped task's trap must exit 1"
        );
        assert_eq!(
            norm(&run.stdout),
            "before\n",
            "{label}: the line printed before the drop survives the trap"
        );
        assert_eq!(
            norm(&run.stderr),
            "error: division by zero\n",
            "{label}: canonical wording, printed exactly once"
        );
    }
}

/// Regression for the RFC-0040 §2 wall (RFC-0023 × RFC-0037): a monomorphized
/// `fn`-value PARAMETER used as a VALUE — stored, not called — must materialize
/// its defunctionalized enum and compile, for ANY payload signature. Storing a
/// fn-param with a NON-SCALAR payload (`fn(User)`, `fn(Validation<User>)`,
/// `fn(Result<User, String>)`) used to emit `error: unbound `cb`` where a scalar
/// `fn(Int64)` built; the fix binds every signature identically. This pins the
/// native build SUCCEEDING and matching the interpreter for each payload shape.
/// Needs clang, so it is `#[ignore]`d like the rest of this file's build tests.
#[test]
#[ignore = "needs clang; run explicitly: cargo test -p vyrn-cli --test parity -- --ignored"]
fn stored_fn_param_compiles_for_any_payload() {
    let out_dir = scratch("parity-fnparam");

    // (payload type, sample value, callback body reading the payload). Each stores
    // `cb` (a fn-param) into module-state Map<String, fn(Payload)>, then retrieves
    // and calls it — the exact shape the std/rpc v2 client emits.
    //
    // `cb: consume Sink` since Phase 10b: a stored `fn` value owns its capture
    // block, so storing a borrowed one is rule 2's refusal like any other store.
    // The generated client says `cb.copy()` instead, because a client reuses one
    // named callback across calls and `consume` refuses the second use.
    let cases: &[(&str, &str, &str, &str)] = &[
        ("scalar", "Int64", "7", "print(\"got: \\{p}\")"),
        ("record", "User", "User { id: 1, name: \"ada\" }", "print(\"got: \\{p.id}/\\{p.name}\")"),
        (
            "validation",
            "Validation<User>",
            "Valid(User { id: 3, name: \"mei\" })",
            "match p { Valid(u) => print(\"valid: \\{u.name}\"), Invalid(i) => print(\"invalid: \\{i.length}\") }",
        ),
        (
            "result",
            "Result<User, String>",
            "Ok(User { id: 42, name: \"zed\" })",
            "match p { Ok(u) => print(\"ok: \\{u.id}\"), Err(e) => print(\"err: \\{e}\") }",
        ),
    ];

    for (label, payload_ty, sample, body) in cases {
        let src = format!(
            "type User = {{ id: Int64, name: String }}\n\
             type Sink = fn({payload_ty})\n\
             let mut pending: Map<String, Sink> = [:]\n\
             fn on(k: String, cb: consume Sink) {{\n    pending[k.copy()] = cb\n}}\n\
             fn fire(k: String, p: {payload_ty}) {{\n    \
             match pending[k] {{ Some(cb) => cb(p), None => print(\"none\") }}\n}}\n\
             fn main() -> Int64 {{\n    on(\"a\", p -> {body})\n    \
             fire(\"a\", {sample})\n    return 0\n}}\n"
        );
        let file = out_dir.join(format!("fnparam_{label}.vyrn"));
        std::fs::write(&file, &src).unwrap();

        let interp = vyrn().arg("run").arg(&file).output().expect("run interp");
        assert!(
            interp.status.success(),
            "{label}: interp must succeed:\n{}",
            norm(&interp.stderr)
        );

        let exe = out_dir.join(format!("fnparam_{label}.exe"));
        let build = vyrn()
            .arg("build")
            .arg(&file)
            .arg("-o")
            .arg(&exe)
            .output()
            .expect("build");
        assert!(
            build.status.success(),
            "{label}: native build of a stored fn-param ({payload_ty}) must succeed \
             (the RFC-0040 §2 wall), got:\n{}{}",
            norm(&build.stdout),
            norm(&build.stderr)
        );
        let native = Command::new(&exe)
            .env("VYRN_FREE_AUDIT", "1")
            .output()
            .expect("run native");
        assert_eq!(
            norm(&native.stdout),
            norm(&interp.stdout),
            "{label}: native stdout must match the interpreter"
        );
        assert_eq!(
            native.status.code(),
            interp.status.code(),
            "{label}: exit code"
        );
    }
}

/// Coverage for a class the main parity loop structurally misses: the SUBDIRECTORY
/// server examples. `examples_dir()` is scanned NON-recursively, so a multi-file
/// app under `examples/<name>/` (`server.vyrn` + its `contract`/`wire`/`routes`
/// modules and generators) is never native-built by any other test. That gap hid
/// an invalid `alloca void` (an inline-constructed generic enum's payload binding
/// stayed `Type::Param`, from the `std/storage` `load(...)` desugar) plus an
/// `Array<fnAlias>` reshape bug (`middleware: Array<Middleware>`) — every server
/// failed at the clang stage, unseen. This pins that each server root compiles all
/// the way to a native object. Build only (the servers are long-running hosts, not
/// run to completion); the reduced runtime behavior lives in `genericpayload.vyrn`,
/// a normal three-way parity citizen. `#[ignore]`d like the rest — needs clang.
#[test]
#[ignore = "needs clang; run explicitly: cargo test -p vyrn-cli --test parity -- --ignored"]
fn subdir_server_examples_native_build() {
    let dir = examples_dir();
    let out_dir = scratch("parity-subdir");

    // Every `examples/<subdir>/server.vyrn` entrypoint, discovered so a future
    // server app is covered automatically.
    let mut servers: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .map(|p| p.join("server.vyrn"))
        .filter(|p| p.exists())
        .collect();
    servers.sort();
    assert!(
        !servers.is_empty(),
        "expected at least one examples/<subdir>/server.vyrn to build"
    );

    for path in &servers {
        let label = path
            .parent()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let exe = out_dir.join(format!("subdir_server_{label}.exe"));
        let build = vyrn()
            .arg("build")
            .arg(path)
            .arg("-o")
            .arg(&exe)
            .output()
            .expect("build server");
        assert!(
            build.status.success(),
            "{label}/server.vyrn: native build must succeed, got:\n{}{}",
            norm(&build.stdout),
            norm(&build.stderr)
        );
    }
}

#[test]
fn expected_check_failures_do_fail() {
    let dir = examples_dir();
    for (name, _why, needle) in EXPECTED_CHECK_FAILURE {
        let path = dir.join(name);
        let out = vyrn().arg("check").arg(&path).output().expect("run check");
        assert!(
            !out.status.success(),
            "{name}: expected `vyrn check` to fail, but it passed"
        );
        let err = norm(&out.stderr) + &norm(&out.stdout);
        assert!(
            err.contains(needle),
            "{name}: expected a diagnostic containing {needle:?}, got:\n{err}"
        );
    }
}

/// The recursion contract, at the largest frame the compiler accepts.
///
/// `CALL_DEPTH_LIMIT` is the language's number and every engine counts to it,
/// but the wasm backend's shadow stack was one 64 KB page and nothing compared a
/// frame against it. So a function with a 256-byte frame ran out of stack at
/// depth 256 — `memory fault at wasm address 0xffffff00`, exit 3 — while the
/// interpreter and the native binary ran the same program to 1,000 and stopped
/// with `error: call depth exceeds 1000`. Two engines reported the limit and the
/// third died 700 frames early with a wild address.
///
/// The stack holds `CALL_DEPTH_LIMIT` frames of `FRAME_LIMIT` now, and a frame
/// past `FRAME_LIMIT` is refused when it is lowered, so the counter always trips
/// first. This proves it near the edge rather than in the middle: `down` holds a
/// 4 KB record, half the frame a function may have, and the wasm build
/// succeeding is the first assertion — a lowering that inflated that frame past
/// the limit would say so here rather than somewhere a user finds it.
///
/// Both sides of the boundary, because a limit that refuses everything would
/// pass the second half alone: under it, one answer on three engines; over it,
/// one diagnostic on three engines.
#[test]
#[ignore = "needs clang and wasmtime; run explicitly: cargo test -p vyrn-cli --test parity -- --ignored"]
fn recursion_with_an_aggregate_local_stops_at_one_limit_on_all_three_engines() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = scratch("framedepth");
    let src = |n: u32| {
        format!(
            "type K8 = {{ a: Int64, b: Int64, c: Int64, d: Int64, e: Int64, f: Int64, \
             g: Int64, h: Int64 }}\n\
             type K64 = {{ a: K8, b: K8, c: K8, d: K8, e: K8, f: K8, g: K8, h: K8 }}\n\
             type K512 = {{ a: K64, b: K64, c: K64, d: K64, e: K64, f: K64, g: K64, h: K64 }}\n\n\
             fn mk8(n: Int64) -> K8 {{\n    \
             return K8 {{ a: n, b: n, c: n, d: n, e: n, f: n, g: n, h: n }}\n}}\n\n\
             fn mk64(n: Int64) -> K64 {{\n    let q = mk8(n)\n    \
             return K64 {{ a: q, b: q, c: q, d: q, e: q, f: q, g: q, h: q }}\n}}\n\n\
             fn mk512(n: Int64) -> K512 {{\n    let q = mk64(n)\n    \
             return K512 {{ a: q, b: q, c: q, d: q, e: q, f: q, g: q, h: q }}\n}}\n\n\
             fn down(n: Int64) -> Int64 {{\n    let big = mk512(n)\n    \
             if n <= 0 {{\n        return big.a.a.a\n    }}\n    \
             return big.a.a.h - big.b.b.h + down(n - 1)\n}}\n\n\
             fn main() -> Int64 {{\n    print(\"\\{{down({n})}}\")\n    return 0\n}}\n"
        )
    };
    // Under the limit, and over it. `down` calls three levels of constructor, so
    // the deep case passes the counter inside `mk8` rather than in `down` — which
    // is the shape a real recursion has and the one the old stack died in.
    for (what, n) in [("under", 200u32), ("over", 1_200u32)] {
        let path = dir.join(format!("{what}.vyrn"));
        std::fs::write(&path, src(n)).unwrap();
        let module = dir.join(format!("{what}.wasm"));
        let build = vyrn()
            .arg("build")
            .arg(&path)
            .arg("--target")
            .arg("wasm")
            .arg("-o")
            .arg(&module)
            .output()
            .expect("build wasm");
        assert!(
            build.status.success(),
            "{what}: a 4 KB record local must still fit one frame — if this is the \
             frame-limit refusal, the lowering grew and the contract's edge moved:\n{}",
            String::from_utf8_lossy(&build.stderr)
        );
        let exe = dir.join(format!("{what}.exe"));
        let nb = vyrn()
            .arg("build")
            .arg(&path)
            .arg("-o")
            .arg(&exe)
            .output()
            .expect("build native");
        assert!(
            nb.status.success(),
            "{what}: native build failed:\n{}",
            String::from_utf8_lossy(&nb.stderr)
        );

        let no_stdin = dir.join("no.stdin");
        let mut interp_cmd = vyrn();
        interp_cmd.arg("run").arg(&path);
        let i = run_io(interp_cmd, &dir, &no_stdin);
        let mut n_cmd = Command::new(&exe);
        n_cmd.env("VYRN_FREE_AUDIT", "1");
        let n_out = run_io(n_cmd, &dir, &no_stdin);
        let mut wasm_cmd = Command::new(&wasmtime);
        wasm_cmd.arg("run").arg(&module);
        let w = run_io(wasm_cmd, &dir, &no_stdin);

        for (other, o) in [("native", &n_out), ("wasm", &w)] {
            assert_eq!(
                runtime_err(&i.stderr),
                runtime_err(&o.stderr),
                "{what}: interp vs {other} stderr"
            );
            assert_eq!(
                norm(&i.stdout),
                norm(&o.stdout),
                "{what}: interp vs {other} stdout"
            );
            assert_eq!(
                i.status.code(),
                o.status.code(),
                "{what}: interp vs {other} exit"
            );
        }
        let limit = vyrn_frontend::interp::CALL_DEPTH_LIMIT;
        if what == "over" {
            assert!(
                runtime_err(&w.stderr).contains(&format!("call depth exceeds {limit}")),
                "over: the deep run must stop at the SHARED limit, not on the shadow \
                 stack — got:\n{}",
                runtime_err(&w.stderr)
            );
            assert_eq!(w.status.code(), Some(1), "over: a trap exits 1");
        } else {
            assert_eq!(norm(&w.stdout).trim(), "0", "under: the answer");
            assert_eq!(w.status.code(), Some(0), "under: a run that fits exits 0");
        }
    }
}

// -------------------------------------------------------------------------
// The wasm cases the corpus does not reach (RFC-0077 M2a-M2p)
// -------------------------------------------------------------------------
/// The one message this backend assembles at runtime rather than interning
/// whole, and the one no example reaches.
///
/// `error: array index 7 out of bounds` has the offending index in the MIDDLE,
/// so it is three writes and an `int_str` rather than a string constant — and a
/// bounds check that never fires reads exactly like one that fires with the
/// wrong wording. Both spellings, because the array and the string paths pick
/// different prefixes.
#[test]
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test parity -- --ignored"]
fn the_bounds_trap_says_what_the_interpreter_says() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = scratch("directwasm-oob");
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
///
/// The `.copy()` calls are RFC-0089 rule 2, and the kernel is what made them
/// necessary (RFC-0125 §3 M3, the default slice). The checker types the
/// GENERIC body, where `T` owns heap or does not according to nothing; the
/// kernel judges the instance, where `T` is `String`. Without them `wrap`
/// stores one `read` parameter into two fields of the same record and
/// `firstOf` returns a field of a `read` parameter — a double free and a lend,
/// visible only after substitution.
#[test]
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test parity -- --ignored"]
fn a_specialization_discovered_from_another_gets_the_index_its_callers_named() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = scratch("directwasm-mono");
    let src = "\
type Pair<A, B> = { first: A, second: B }

fn wrap<T>(x: T) -> Pair<T, T> {
    return Pair { first: x.copy(), second: x.copy() }
}

fn twice<T>(x: T) -> Pair<T, T> {
    return wrap(x)
}

fn firstOf<A, B>(p: Pair<A, B>) -> A {
    return p.first.copy()
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
        .output()
        .expect("build wasm");
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let mut interp_cmd = vyrn();
    interp_cmd.arg("run").arg(&path);
    let interp = run_io(interp_cmd, &dir, &dir.join("no.stdin"));
    let mut wasm_cmd = Command::new(&wasmtime);
    wasm_cmd.arg("run").arg(&module);
    let w = run_io(wasm_cmd, &dir, &dir.join("no.stdin"));

    // A merged specialization is the failure this is really about: `twice<Int64>`
    // and `twice<String>` are the same source and different code, and merging
    // them prints a plausible number where a string belongs.
    assert_eq!(
        norm(&interp.stdout),
        "42\nhi\ntrue\n",
        "the interpreter moved"
    );
    assert_eq!(norm(&interp.stdout), norm(&w.stdout), "stdout");
    assert_eq!(
        runtime_err(&interp.stderr),
        runtime_err(&w.stderr),
        "stderr"
    );
    assert_eq!(interp.status.code(), w.status.code(), "exit");
}

/// The three things the `=~` walk (RFC-0077 M2m) can get wrong that the whole
/// corpus is blind to, because every `=~` in `examples/` and `std/` runs over
/// ASCII.
///
/// The load of the input byte has to be **unsigned**. A signed one turns a UTF-8
/// continuation byte into a negative table index, which reads memory *below* the
/// transition table and answers wrongly — no trap, because the table sits in the
/// middle of a live address space. Checked by breaking it: with `i32.load8_s`,
/// `regex`, `finitekeys`, `i18ndemo` and `twdemo` all still pass, and the two
/// non-ASCII lines here go false where the interpreter says true and true where it
/// says false.
///
/// The other two are the zero-length walk — the answer is whether the START state
/// accepts, which a do-while shape gets wrong and no example asks — and a
/// non-match that keeps walking after it is already lost, which is what the dead
/// state absorbing every remaining byte means.
///
/// Pinned against the interpreter's answer, not against a spelling written here:
/// `Dfa::matches` is the third walk over the same table and the one the checker
/// already trusts.
#[test]
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test parity -- --ignored"]
fn the_dfa_walk_agrees_with_the_interpreter_on_what_no_example_reaches() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = scratch("directwasm-regex");
    let src = "\
fn main() -> Int64 {
    print(\"\" =~ \"a*\")
    print(\"\" =~ \"a+\")
    print(\"b\" =~ \"a*\")
    print(\"é\" =~ \".\")
    print(\"é\" =~ \"..\")
    print(\"café\" =~ \"caf.\")
    print(\"café\" =~ \"caf..\")
    print(\"ünïcödé\" =~ \".*\")
    print(\"abcXdefghijklmnop\" =~ \"[a-z]+\")
    return 0
}
";
    let path = dir.join("rx.vyrn");
    std::fs::write(&path, src).unwrap();
    let module = dir.join("rx.wasm");
    let build = vyrn()
        .arg("build")
        .arg(&path)
        .arg("--target")
        .arg("wasm")
        .arg("-o")
        .arg(&module)
        .output()
        .expect("build wasm");
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let mut interp_cmd = vyrn();
    interp_cmd.arg("run").arg(&path);
    let interp = run_io(interp_cmd, &dir, &dir.join("no.stdin"));
    let mut wasm_cmd = Command::new(&wasmtime);
    wasm_cmd.arg("run").arg(&module);
    let w = run_io(wasm_cmd, &dir, &dir.join("no.stdin"));

    // Spelled out so a change in what the LANGUAGE answers is a failure here too,
    // not just a change in what the two engines answer together. `"é" =~ "."` is
    // false because `.` is one BYTE and `é` is two — RFC-0046 runs a byte DFA, and
    // that is the fact both non-ASCII lines are really pinning.
    assert_eq!(
        norm(&interp.stdout),
        "true\nfalse\nfalse\nfalse\ntrue\nfalse\ntrue\ntrue\nfalse\n",
        "the interpreter moved"
    );
    assert_eq!(norm(&interp.stdout), norm(&w.stdout), "stdout");
    assert_eq!(
        runtime_err(&interp.stderr),
        runtime_err(&w.stderr),
        "stderr"
    );
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
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test parity -- --ignored"]
fn a_modify_parameter_copies_back_whatever_the_caller_kept_it_in() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = scratch("directwasm-modify");
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
        .output()
        .expect("build wasm");
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );

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
    assert_eq!(
        runtime_err(&interp.stderr),
        runtime_err(&w.stderr),
        "stderr"
    );
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
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test parity -- --ignored"]
fn a_boxed_enum_payload_survives_the_word_it_travels_in() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = scratch("directwasm-value");
    let src = "\
fn show(v: Value) -> String {
    return match v {
        IntVal(n) => n.toString(),
        BoolVal(b) => b.toString(),
        StrVal(s) => s.copy(),
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
        .output()
        .expect("build wasm");
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let mut interp_cmd = vyrn();
    interp_cmd.arg("run").arg(&path);
    let interp = run_io(interp_cmd, &dir, &dir.join("no.stdin"));
    let mut wasm_cmd = Command::new(&wasmtime);
    wasm_cmd.arg("run").arg(&module);
    let w = run_io(wasm_cmd, &dir, &dir.join("no.stdin"));

    // Spelled out, because the failure was a plausible-looking string rather than
    // a crash: garbage bytes where "hi there" belonged, and a byte count where a
    // character count belonged.
    assert_eq!(
        norm(&interp.stdout),
        "hi there\ntrue\n-7\n5\n6\n0\n",
        "the interpreter moved"
    );
    assert_eq!(norm(&interp.stdout), norm(&w.stdout), "stdout");
    assert_eq!(
        runtime_err(&interp.stderr),
        runtime_err(&w.stderr),
        "stderr"
    );
    assert_eq!(interp.status.code(), w.status.code(), "exit");
}

/// A temporary handed to a position that KEEPS it, on all three engines
/// (`rfcs/census-call-arguments.md`).
///
/// The call-argument rule releases a temporary after a call whose parameter is
/// `read` and keeps nothing. Every OTHER verdict must free nothing at the call,
/// and if the rule overreached this program would free one buffer twice — once
/// at the call and once when the tree is released — which the memory suite
/// cannot see, because a double free is not a leak. The output can: every tree
/// is printed AFTER the call that would have freed it, so a reused buffer shows
/// as wrong bytes, and a hardened allocator traps instead.
///
/// Four shapes, one per verdict the caller must not act on:
///
/// - `Transferred` — `tip(s: consume String)`. The builder TAKES its argument,
///   which is what a value holding it past the call means. It was a `read`
///   parameter here until the constructor hole was closed: `return Tip(s)` on a
///   borrow lends the result as well, so neither the argument nor the result
///   could be freed and both leaked (the census's finding 2, 48.8 MB over a
///   million turns). The declaration is the fix, and `check_return` refuses the
///   old spelling now.
/// - `Transferred`, forwarded — `relabel` hands its own taken value on to `tip`.
/// - `Retained` by a constructor — `Fork(label(i + 2), [])`, the literal that
///   reads like a call.
/// - `Retained` by a recorded position — `stash(s)` puts a borrowed parameter
///   into a tree this module KEEPS through the `.copy()` the constructor
///   position demands since exit-residue round ten (the bare borrow is
///   refused now), so `note_retention` records `(stash, 0)`, and `restash`
///   forwards into it, which is the "handed on" edge the retention set
///   closes over the call graph.
///
/// It carries the operator class too (`rfcs/census-call-arguments.md` §9,
/// finding 3): `show` builds `s + "/\{kids.length}"`, so a `+` runs over an
/// operand a call produced on every engine.
#[test]
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test parity -- --ignored"]
fn a_retained_argument_is_not_freed_at_the_call() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = scratch("retained-arg");
    let src = "\
type Twig =
    | Tip(String)
    | Fork(String, Array<Twig>)

impl Owned for Twig {
    fn release(consume self) {
        match consume self {
            Tip(s) => {
                drop s
            }
            Fork(s, kids) => {
                drop s
                drop kids
            }
        }
    }
}

let mut kept: Array<Twig> = []

fn label(i: Int64) -> String {
    return \"row-\\{i}\"
}

fn show(t: Twig) -> String {
    return match t {
        Tip(s) => s.copy(),
        Fork(s, kids) => s + \"/\\{kids.length}\",
    }
}

/// The value it builds holds the argument and outlives the call, so it TAKES
/// it. The verdict at the call is `Transferred`, and the caller frees nothing.
fn tip(s: consume String) -> Twig {
    return Tip(s)
}

/// The forwarded one: a taken value handed on to another taking position.
fn relabel(s: consume String) -> Twig {
    return tip(s)
}

/// The RECORDED position — a `read` parameter put somewhere that outlives the
/// call, which is what `movecheck::note_retention` writes down. Nothing is
/// returned, so the tree this module keeps is the only owner.
fn stash(s: String) -> Int64 {
    kept.push(Tip(s.copy()))
    return Int64(kept.length)
}

/// The edge the retention set closes over the call graph.
fn restash(s: String) -> Int64 {
    return stash(s)
}

fn main() -> Int64 {
    let mut i: Int64 = 0
    let mut total: Int64 = 0
    while i < 200 {
        let a = tip(label(i))
        let b = relabel(label(i + 1))
        let c = Fork(label(i + 2), [])
        // Every read happens AFTER the calls that would have freed too early.
        total = total + Int64(show(a).byteLength) + Int64(show(b).byteLength)
        total = total + Int64(show(c).byteLength)
        total = total + stash(label(i + 3)) + restash(label(i + 4))
        i = i + 1
    }
    print(\"\\{total}\")
    print(show(tip(label(7))))
    print(show(relabel(label(8))))
    // The kept trees, read long after the calls that built them.
    print(show(kept[0]))
    print(show(kept[399]))
    return 0
}
";
    let path = dir.join("retained.vyrn");
    std::fs::write(&path, src).unwrap();
    let module = dir.join("retained.wasm");
    let build = vyrn()
        .arg("build")
        .arg(&path)
        .arg("--target")
        .arg("wasm")
        .arg("-o")
        .arg(&module)
        .output()
        .expect("build wasm");
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = dir.join("retained.exe");
    let nb = vyrn()
        .arg("build")
        .arg(&path)
        .arg("-o")
        .arg(&native)
        .output()
        .expect("build native");
    assert!(
        nb.status.success(),
        "{}",
        String::from_utf8_lossy(&nb.stderr)
    );

    let mut interp_cmd = vyrn();
    interp_cmd.arg("run").arg(&path);
    let interp = run_io(interp_cmd, &dir, &dir.join("no.stdin"));
    let n = run_io(Command::new(&native), &dir, &dir.join("no.stdin"));
    let mut wasm_cmd = Command::new(&wasmtime);
    wasm_cmd.arg("run").arg(&module);
    let w = run_io(wasm_cmd, &dir, &dir.join("no.stdin"));

    // Spelled out: a use-after-free here is a plausible-looking string, not a
    // crash, so the expected bytes are written down rather than compared engine
    // to engine alone.
    assert_eq!(
        norm(&interp.stdout),
        "84476\nrow-7\nrow-8\nrow-3\nrow-203\n",
        "the interpreter moved"
    );
    assert_eq!(norm(&interp.stdout), norm(&n.stdout), "native stdout");
    assert_eq!(norm(&interp.stdout), norm(&w.stdout), "wasm stdout");
    assert_eq!(
        runtime_err(&interp.stderr),
        runtime_err(&n.stderr),
        "native"
    );
    assert_eq!(runtime_err(&interp.stderr), runtime_err(&w.stderr), "wasm");
    assert_eq!(interp.status.code(), n.status.code(), "native exit");
    assert_eq!(interp.status.code(), w.status.code(), "wasm exit");
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
/// byte are here for that, not for coverage.
///
/// `slice`'s two failures used to be separate programs because a trap ended the
/// run; RFC-0079 M3 made them VALUES, so they moved into the `ok` case beside
/// everything else and the `traps` case is gone. The pin got stronger for free —
/// the interpreter's own answer is asserted for the failing ranges too, where
/// before only "the stderr mentioned slice" could be.
#[test]
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test parity -- --ignored"]
fn the_string_builtins_agree_with_the_interpreter_about_their_failures() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = scratch("directwasm-strbytes");

    let show = "\
import { SliceError, slice } from \"std/strpred\"
fn show(r: Result<String, String>) -> String {
    return match r {
        Ok(s) => \"ok:\" + s,
        Err(e) => \"err:\" + e,
    }
}
fn sl(s: String, a: Int64, b: Int64) -> String {
    return match slice(s, a, b) {
        Ok(v) => v,
        Err(e) => match e {
            OutOfRange(i) => \"oob:\\{i}\",
            SplitsCharacter(i) => \"split:\\{i}\",
        },
    }
}
";
    let cases: [(&str, &str); 2] = [
        (
            "ok",
            "\
fn main() -> Int64 {
    print(show(stringFromBytes(bytes(\"héllo\"))))
    print(show(stringFromBytes([]))) // the empty buffer is a valid empty String
    print(show(stringFromBytes(['\\xf0', '\\x9f', '\\x98', '\\x80'])))
    print(sl(\"héllo wörld\", 0, 6))
    print(sl(\"héllo\", 6, 6))     // `to == len` is the byte length, a boundary
    print(sl(\"hi\", 0, 9))          // end past the length
    print(sl(\"hé\", 0, 2))          // byte 2 is é's continuation byte
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
    ];
    for (what, body) in cases {
        let (name, src) = (what.to_string(), format!("{show}{body}"));
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
            .output()
            .expect("build wasm");
        assert!(
            build.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&build.stderr)
        );

        let mut interp_cmd = vyrn();
        interp_cmd.arg("run").arg(&path);
        let interp = run_io(interp_cmd, &dir, &dir.join("no.stdin"));
        let mut wasm_cmd = Command::new(&wasmtime);
        wasm_cmd.arg("run").arg(&module);
        let w = run_io(wasm_cmd, &dir, &dir.join("no.stdin"));

        assert_eq!(norm(&interp.stdout), norm(&w.stdout), "{name}: stdout");
        assert_eq!(
            runtime_err(&interp.stderr),
            runtime_err(&w.stderr),
            "{name}: stderr"
        );
        assert_eq!(interp.status.code(), w.status.code(), "{name}: exit");
        // Comparing two backends would pass if both were silently wrong about
        // which failure happened, so the interpreter's own answer is pinned.
        match what {
            "ok" => assert_eq!(
                norm(&interp.stdout),
                "ok:héllo\nok:\nok:😀\nhéllo\n\noob:9\nsplit:2\n3\n",
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
            other => panic!("unknown case `{other}`"),
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
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test parity -- --ignored"]
fn every_integer_width_wraps_where_the_interpreter_wraps() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = scratch("directwasm-ints");

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
        .output()
        .expect("build wasm");
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );

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
    assert_eq!(
        runtime_err(&interp.stderr),
        runtime_err(&w.stderr),
        "stderr"
    );
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
///
/// Since RFC-0081 M2 the wasm column is `std/num`'s `f64Str` rather than 511
/// hand-written lines, so what this pins there is the same six places produced by
/// a different implementation — and the `want` string, which is neither engine's
/// output but a value someone wrote down, is what makes that a check rather than
/// a comparison of two copies.
#[test]
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test parity -- --ignored"]
fn six_decimals_of_a_float_are_the_exact_ones() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = scratch("directwasm-floats");
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
        .output()
        .expect("build wasm");
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );

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
    assert_eq!(
        runtime_err(&interp.stderr),
        runtime_err(&w.stderr),
        "stderr"
    );
    assert_eq!(interp.status.code(), w.status.code(), "exit");
}

/// `std/num:f64Str` against the builtin `@str` on every engine (RFC-0081 M1).
///
/// The test above pins the six places on values someone chose. This one is the
/// differential: 400 doubles from a fixed generator, each formatted twice —
/// once by the Vyrn function and once by whatever that engine's `@str` is — and
/// compared inside the program, so each engine's answer is checked against ITS
/// OWN reference. That matters here more than anywhere else in this file,
/// because the three references are not one implementation compiled three ways:
/// they are Rust's `{:.6}`, C's `printf("%f")` and 511 lines of hand-written
/// wasm, and M1 exists to find out whether one Vyrn function can replace all
/// three.
///
/// Raw bit patterns for half of it, which is how the corpus reaches subnormals,
/// both zeros, every exponent and the three non-finite spellings — none of which
/// a literal in this language can name. The other half forces the exponent into
/// the range programs actually print, because a rounding bug in the everyday
/// magnitudes is the one that would be seen.
///
/// The mismatch count is printed rather than asserted so a disagreement arrives
/// as a diff naming the value, and so an engine that produced nothing at all
/// fails rather than passing quietly.
///
/// **M2 changed what two of the three columns mean, and the test is worth more
/// for it.** `@str` on a float IS `f64Str` now on native and on wasm, so their
/// in-program comparison is a function against itself — the differential that
/// remains is the interpreter's, where `@str` is still `format!("{f:.6}")`. That
/// is the arrangement M2 chose deliberately: one implementation and one oracle,
/// with a test enforcing the relation, rather than three peers with no reference
/// among them. The `all_agree` at the end is what still checks the two compiled
/// engines — against the interpreter's bytes, which are the oracle's.
#[test]
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test parity -- --ignored"]
fn the_vyrn_float_formatter_agrees_with_every_engines_own() {
    let rows = three_engines(
        "f64str",
        "f64str",
        r#"
import { f64Str } from "std/num"

fn main() -> Int64 {
    let mut s: UInt64 = 11400714819323198485
    let mut bad = 0
    let mut n = 0
    while n < 200 {
        s = s * 6364136223846793005 + 1442695040888963407
        let x = floatFromBits(s)
        let a = f64Str(x)
        let b = x.toString()
        if a != b {
            print("MISMATCH raw \{n} builtin=\{b} f64Str=\{a}")
            bad = bad + 1
        }
        // The same bits with the exponent field replaced: an everyday magnitude,
        // sign and mantissa still arbitrary.
        let e2: UInt64 = 990 + (s >> 40) % 64
        let y = floatFromBits((s & 4503599627370495) | (e2 << 52) | ((s >> 63) << 63))
        let c = f64Str(y)
        let d = y.toString()
        if c != d {
            print("MISMATCH scaled \{n} builtin=\{d} f64Str=\{c}")
            bad = bad + 1
        }
        n = n + 1
    }
    print("\{bad} mismatches of \{n * 2}")
    // Printed as bytes as well, so the engines are compared against each other
    // and not only each against its own reference.
    print(f64Str(0.0078125))
    print(f64Str(0.0234375))
    print(f64Str(floatFromBits(9223372036854775808)))
    print(f64Str(floatFromBits(1)))
    // The four spelled values, by bits — the random half above reaches an
    // exponent field of 2047 only about a fifth of the time, and a NEGATIVE NaN
    // is the row where the two references disagree about what to say.
    print(f64Str(floatFromBits(9218868437227405312)))
    print(f64Str(floatFromBits(18442240474082181120)))
    print(f64Str(floatFromBits(9221120237041090560)))
    print(f64Str(floatFromBits(18444492273895866368)))
    return 0
}
"#,
    );
    assert!(
        rows.iter().any(|(e, _, _, _)| *e == "wasm"),
        "no wasm column: wasmtime did not resolve, so this proved nothing about the 511 lines"
    );
    for (eng, out, _, _) in &rows {
        assert!(
            out.contains("0 mismatches of 400"),
            "{eng} disagrees with its own `@str`:\n{out}"
        );
    }
    all_agree(&rows, "f64str");
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
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test parity -- --ignored"]
fn the_wasi_io_builtins_agree_with_the_interpreter_about_their_edges() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = scratch("directwasm-wasiio");
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
    // RFC-0044's `renameFile` (M2p). Self-setting-up, because the interpreter and
    // the wasm module run in the SAME directory one after the other and a rename is
    // destructive: both runs write both files first, so both see the same world.
    let rename = "\
fn show(r: Result<Bool, String>) -> String {
    return match r {
        Ok(b) => \"ok:\\{b}\",
        Err(e) => \"err:\" + e,
    }
}

fn read(p: String) -> String {
    return match readFile(p) {
        Ok(s) => \"ok:\" + s,
        Err(e) => \"err:\" + e,
    }
}

fn main() -> Int64 {
    print(show(writeFile(\"rn-from.txt\", \"moved\")))
    print(show(writeFile(\"rn-onto.txt\", \"clobbered\")))
    print(show(renameFile(\"rn-from.txt\", \"rn-onto.txt\")))
    print(read(\"rn-onto.txt\"))
    print(read(\"rn-from.txt\"))
    print(show(renameFile(\"rn-missing.txt\", \"rn-onto.txt\")))
    print(show(renameFile(\"rn-onto.txt\", \"rn-nodir/x.txt\")))
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
        (
            "nulline",
            lines,
            Some(stdin_bad),
            no_args.clone(),
            "1 [good] 6\nlines 1\n",
        ),
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
            "rename",
            rename,
            None,
            no_args.clone(),
            // Line 3 says it overwrote an existing target — which POSIX `rename`
            // and `path_rename` do and Windows C `rename` refuses, so it is the
            // semantic RFC-0044 is about. Line 5 says it MOVED rather than copied.
            // The two failures are the reachable half of the two error classes: a
            // missing source and an unresolvable target are both `cannot write`
            // ABOUT THE TARGET, and the cross-device wording is the arm nothing
            // here can reach, a preopen being one mount.
            "ok:true\nok:true\nok:true\nok:moved\n\
             err:cannot read `rn-from.txt`\n\
             err:cannot write `rn-onto.txt`\n\
             err:cannot write `rn-nodir/x.txt`\n",
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
            .output()
            .expect("build wasm");
        assert!(
            build.status.success(),
            "{what}: {}",
            String::from_utf8_lossy(&build.stderr)
        );

        let mut interp_cmd = vyrn();
        interp_cmd.arg("run").arg(&path).args(&prog_args);
        let interp = run_io(interp_cmd, &dir, &stdin_path);
        let mut wasm_cmd = Command::new(&wasmtime);
        wasm_cmd
            .arg("run")
            .arg("--dir")
            .arg(".")
            .arg(&module)
            .args(&prog_args);
        let w = run_io(wasm_cmd, &dir, &stdin_path);

        assert_eq!(norm(&interp.stdout), want, "{what}: the interpreter moved");
        assert_eq!(norm(&interp.stdout), norm(&w.stdout), "{what}: stdout");
        assert_eq!(
            runtime_err(&interp.stderr),
            runtime_err(&w.stderr),
            "{what}: stderr"
        );
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
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test parity -- --ignored"]
fn a_propagating_early_exit_releases_its_frame_and_copies_modify_back() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = scratch("directwasm-try");
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
        .output()
        .expect("build wasm");
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let mut interp_cmd = vyrn();
    interp_cmd.arg("run").arg(&path);
    let interp = run_io(interp_cmd, &dir, &dir.join("no.stdin"));
    let mut wasm_cmd = Command::new(&wasmtime);
    wasm_cmd.arg("run").arg(&module);
    let w = run_io(wasm_cmd, &dir, &dir.join("no.stdin"));

    assert_eq!(
        norm(&interp.stdout),
        "20000\n20101\n",
        "the interpreter moved"
    );
    assert_eq!(norm(&interp.stdout), norm(&w.stdout), "stdout");
    assert_eq!(
        runtime_err(&interp.stderr),
        runtime_err(&w.stderr),
        "stderr"
    );
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
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test parity -- --ignored"]
fn the_json_reader_parses_the_same_on_the_direct_backend() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = scratch("directwasm-jsonread");
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
        .output()
        .expect("build wasm");
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );

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
    assert_eq!(
        runtime_err(&interp.stderr),
        runtime_err(&w.stderr),
        "stderr"
    );
    assert_eq!(interp.status.code(), w.status.code(), "exit");
}

/// The RFC-0023 shapes the corpus does not reach (RFC-0077 M2m).
///
/// `lambdas`, `rpc` and `rpcsplit` between them exercise a scalar capture, a named
/// function as a target, and a pass-through whose target has NO captures. Every
/// other shape a `fn`-typed parameter has is invisible to the ladder, and each one
/// here is silent when wrong rather than loud:
///
/// - An **aggregate capture**, and a callee parameter with the SAME NAME as it. A
///   specialization's capture parameters are `@cap..`, which no Vyrn identifier can
///   be; a spelling that could collide would bind `p` inside the lambda to the
///   callee's own `p` and print a plausible number.
/// - The same capture through **two boundaries** (`via` forwards its `fn`
///   parameter to `on`), which is what says a forwarded target carries its
///   captures rather than re-reading them.
/// - An **aggregate parameter and an aggregate return** on the `fn` type: the
///   argument is an address and the return a hidden leading destination, so the
///   convention has to reach a target call, not just an ordinary one.
/// - **Two distinct lambdas of the same shape** at two sites: two instances. One
///   shared instance would print the first lambda's answer twice.
/// - **One literal inside a generic body, two instantiations**: two lifted copies.
///   Sharing one would hand the `Int64` copy a `String`.
/// - A **Unit-returning** `fn` type over an expression body, which is the rpc
///   callback shape written the short way: the value is a statement, not a return.
/// - A **block-bodied** lambda, two `fn` parameters in one specialization, and a
///   `fn` parameter called three times in a loop.
#[test]
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test parity -- --ignored"]
fn a_fn_typed_parameter_specializes_to_whatever_the_call_site_resolved() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = scratch("directwasm-ho");
    let src = "\
type Pt = { x: Int64, y: Int64 }

fn on(p: Pt, f: fn(Pt) -> Pt) -> Pt {
    return f(p)
}

/// A pass-through whose target carries captures, so they travel two boundaries.
fn via(p: Pt, f: fn(Pt) -> Pt) -> Pt {
    return on(p, f)
}

fn flip(p: Pt) -> Pt {
    return Pt { x: p.y, y: p.x }
}

fn foldOver<T, A>(xs: Array<T>, init: A, f: fn(A, T) -> A) -> A {
    let mut acc = init
    for x in xs {
        acc = f(acc, x)
    }
    return acc
}

/// One lambda literal, two instantiations, two lifted copies.
fn countAll<T>(xs: Array<T>) -> Int64 {
    return foldOver(xs, 0, (acc, x) -> acc + 1)
}

fn both(n: Int64, f: fn(Int64) -> Int64, g: fn(Int64) -> Int64) -> Int64 {
    return f(n) * 100 + g(n)
}

fn thrice(n: Int64, f: fn(Int64) -> Int64) -> Int64 {
    let mut acc = 0
    let mut i = 0
    while i < 3 {
        acc = acc + f(n + i)
        i = i + 1
    }
    return acc
}

fn each(xs: Array<Int64>, f: fn(Int64)) {
    for x in xs {
        f(x)
    }
}

fn main() -> Int64 {
    // An aggregate capture named exactly as `on`'s own first parameter is.
    let p = Pt { x: 10, y: 20 }
    let a = on(Pt { x: 1, y: 2 }, q -> Pt { x: q.x + p.x, y: q.y + p.y })
    print(a.x * 100 + a.y)
    let b = via(Pt { x: 3, y: 4 }, q -> Pt { x: q.x + p.x, y: q.y + p.y })
    print(b.x * 100 + b.y)
    let c = on(Pt { x: 5, y: 6 }, flip)
    print(c.x * 100 + c.y)

    let nums: Array<Int64> = [1, 2, 3]
    print(foldOver(nums, 0, (acc, x) -> acc + x))
    print(foldOver(nums, 0, (acc, x) -> acc + x * 10))
    let words: Array<String> = [\"a\", \"b\"]
    print(countAll(nums) + countAll(words))

    let u = 2
    let v = 5
    print(both(3, x -> x + u, x -> x * v))
    print(thrice(10, x -> { let d = x * 2 return d + 1 }))
    // One literal reached twice at one site: one instance.
    print(thrice(1, x -> x) + thrice(1, x -> x))

    let tag = \"n=\"
    each(nums, x -> print(\"\\{tag}\\{x}\"))
    return 0
}
";
    let path = dir.join("ho.vyrn");
    std::fs::write(&path, src).unwrap();
    let module = dir.join("ho.wasm");
    let build = vyrn()
        .arg("build")
        .arg(&path)
        .arg("--target")
        .arg("wasm")
        .arg("-o")
        .arg(&module)
        .output()
        .expect("build wasm");
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let mut interp_cmd = vyrn();
    interp_cmd.arg("run").arg(&path);
    let interp = run_io(interp_cmd, &dir, &dir.join("no.stdin"));
    let mut wasm_cmd = Command::new(&wasmtime);
    wasm_cmd.arg("run").arg(&module);
    let w = run_io(wasm_cmd, &dir, &dir.join("no.stdin"));

    // Pinned to the INTERPRETER's answers, because two backends can be
    // confidently wrong together — a merged specialization prints one lambda's
    // result for both.
    assert_eq!(
        norm(&interp.stdout),
        "1122\n1324\n605\n6\n60\n5\n515\n69\n12\nn=1\nn=2\nn=3\n",
        "the interpreter moved"
    );
    assert_eq!(norm(&interp.stdout), norm(&w.stdout), "stdout");
    assert_eq!(
        runtime_err(&interp.stderr),
        runtime_err(&w.stderr),
        "stderr"
    );
    assert_eq!(interp.status.code(), w.status.code(), "exit");
}

/// The RFC-0037 shapes `closures2` and `fnvalstore` do not reach (RFC-0077 M2m).
///
/// Between them those two are most of the stored-`fn` surface — a record capture,
/// a `Validation<Record>` payload, storage in a Map, an Array, a record field and
/// module state, an aggregate return through `Middleware`, a stored value flowing
/// into a `fn`-typed parameter, and a trap inside a closure. Three things they do
/// not have, each silent when wrong:
///
/// - A **Unit-signature slot holding a value-returning function**. The dispatcher
///   has to drop the result; leaving it on the stack is a module wasmtime refuses,
///   but dropping the wrong one is not.
/// - An **aggregate return from a lifted lambda** through a dispatcher, where the
///   result travels through the dispatcher's own hidden destination rather than as
///   a value.
/// - **Two spellings of one signature.** `Make` and the bare `fn(Int64) -> Pt`
///   must register and dispatch as ONE enum, or a tag built under one spelling
///   falls through the other's switch to the defensive arm — which is exactly what
///   `normalize_fn_sig` is shared with the textual backend for.
#[test]
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test parity -- --ignored"]
fn a_stored_function_value_dispatches_by_signature_not_by_spelling() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = scratch("directwasm-fnval");
    let src = "\
type Pt = { x: Int64, y: Int64 }

type Sink = fn(Int64)

type Make = fn(Int64) -> Pt

fn shout(n: Int64) -> Int64 {
    print(\"shout \\{n}\")
    return n * 2
}

fn origin(n: Int64) -> Pt {
    return Pt { x: n, y: 0 - n }
}

fn main() -> Int64 {
    let s: Sink = shout
    s(4)
    let mk: Make = origin
    let a = mk(3)
    print(a.x * 100 + a.y)
    let lam: Make = n -> Pt { x: n + 1, y: n + 2 }
    let b = lam(10)
    print(b.x * 100 + b.y)
    let raw: fn(Int64) -> Pt = lam
    let c = raw(20)
    print(c.x * 100 + c.y)
    return 0
}
";
    let path = dir.join("fnval.vyrn");
    std::fs::write(&path, src).unwrap();
    let module = dir.join("fnval.wasm");
    let build = vyrn()
        .arg("build")
        .arg(&path)
        .arg("--target")
        .arg("wasm")
        .arg("-o")
        .arg(&module)
        .output()
        .expect("build wasm");
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let mut interp_cmd = vyrn();
    interp_cmd.arg("run").arg(&path);
    let interp = run_io(interp_cmd, &dir, &dir.join("no.stdin"));
    let mut wasm_cmd = Command::new(&wasmtime);
    wasm_cmd.arg("run").arg(&module);
    let w = run_io(wasm_cmd, &dir, &dir.join("no.stdin"));

    assert_eq!(
        norm(&interp.stdout),
        "shout 4\n297\n1112\n2122\n",
        "the interpreter moved"
    );
    assert_eq!(norm(&interp.stdout), norm(&w.stdout), "stdout");
    assert_eq!(
        runtime_err(&interp.stderr),
        runtime_err(&w.stderr),
        "stderr"
    );
    assert_eq!(interp.status.code(), w.status.code(), "exit");
}

/// The three things a `Task` (RFC-0025) can be that no example makes one of.
///
/// `concurrency` and `parallel` between them only ever spawn a `Task<Int64>` and
/// join it in the same frame that made it, so three parts of the lowering are
/// unreached by the ladder and each would be silent:
///
/// - **Four live `Task`s at once.** The result is boxed on the heap for exactly
///   this. A frame slot would be handed out ONCE per function — `Frame::alloc`
///   offsets are never reused, so a slot inside a loop is one slot — and all four
///   tasks would be the same address: the stack-slot version of this file prints
///   `233` four times where the interpreter prints `55 89 144 233`. Checked by
///   building it that way, because a lifetime bug that no example reaches is
///   exactly the class this RFC keeps finding by running things.
/// - **A `Task` of an aggregate**, where `join` copies rather than handing out the
///   box's own address — the `load {ll}` the LLVM backend emits, and M2l's `get`
///   hazard one container along.
/// - **A `Task<Unit>`**, which has no result to read and still has to be a value
///   `join` can consume.
///
/// Pinned against the interpreter, not against numbers written here: eager
/// evaluation at the spawn point is the interpreter's own schedule, so there is
/// one right answer and it is the one the interpreter gives.
#[test]
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test parity -- --ignored"]
fn a_task_that_escapes_its_frame_says_what_the_interpreter_says() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = scratch("directwasm-spawn");
    for (what, src) in [
        // Four tasks spawned in a loop and all joined afterwards, so four boxes
        // have to coexist.
        //
        // The joins walk the array with `for … in consume` since RFC-0095 M1. A
        // `Task<T>` is linear and RFC-0092 M4 carries that obligation through
        // the container, so the array must be handed on by name: `consume` gives
        // the loop the array, and every element is joined once inside it. Four
        // `ts[i].join()` reads left the container undischarged and read one
        // element four times over — which is also what a second join of one
        // element would be, now that a join frees the box. The loop variable is
        // not `t`: `Consumed` is keyed by NAME, so an inner `t` shadowing the
        // `t` that was pushed reads as a use after that move.
        (
            "escaping",
            "fn fib(n: Int64) -> Int64 {\n if n < 2 { return n }\n \
             return fib(n - 1) + fib(n - 2)\n}\n\
             fn main() -> Int64 {\n let mut ts: Array<Task<Int64>> = []\n \
             let mut i = 0\n while i < 4 {\n let t = spawn fib(i + 10)\n \
             ts = ts.push(t)\n i = i + 1\n }\n \
             for one in consume ts {\n print(one.join())\n }\n return 0\n}\n",
        ),
        // An aggregate result must be COPIED out of the box, not aliased into.
        //
        // It used to be two joins of one task, with the first result mutated in
        // between. A task is linear now, so the second join is a compile error
        // and the hazard is pinned with a second task instead: the first box is
        // freed at its join, an allocator hands the same address to the second
        // spawn, and a `p` that aliased the first box would follow the second
        // task's result. That is both faults at once — the alias and the read of
        // a freed box.
        (
            "aggregate",
            "type P = { a: Int64, b: Int64 }\n\
             fn mk(x: Int64) -> P {\n return P { a: x, b: x * 2 }\n}\n\
             fn main() -> Int64 {\n let t = spawn mk(5)\n let mut p = t.join()\n \
             p.a = 99\n let u = spawn mk(7)\n let q = u.join()\n \
             print(p.a)\n print(p.b)\n print(q.a)\n print(q.b)\n \
             return 0\n}\n",
        ),
        (
            "unit",
            "fn noop(x: Int64) {\n let y = x + 1\n}\n\
             fn main() -> Int64 {\n let t = spawn noop(3)\n t.join()\n print(1)\n return 0\n}\n",
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
            .output()
            .expect("build wasm");
        assert!(
            build.status.success(),
            "{what}: {}",
            String::from_utf8_lossy(&build.stderr)
        );

        let mut interp_cmd = vyrn();
        interp_cmd.arg("run").arg(&path);
        let interp = run_io(interp_cmd, &dir, &dir.join("no.stdin"));
        let mut wasm_cmd = Command::new(&wasmtime);
        wasm_cmd.arg("run").arg(&module);
        let w = run_io(wasm_cmd, &dir, &dir.join("no.stdin"));

        assert_eq!(norm(&interp.stdout), norm(&w.stdout), "{what}: stdout");
        assert_eq!(
            runtime_err(&interp.stderr),
            runtime_err(&w.stderr),
            "{what}: stderr"
        );
        assert_eq!(interp.status.code(), w.status.code(), "{what}: exit");
        // A pass with no output at all would be two engines agreeing on nothing.
        assert!(!norm(&w.stdout).is_empty(), "{what}: printed nothing");
    }
}

/// Every edge that leaves a `region`, taken more often than the region stack is
/// deep — and the depth bound itself, reached across calls.
///
/// This backend's region is a counter and a trap (RFC-0004 §4's arena is the bump
/// allocator's ceiling, not a region-shaped hole — see `Fn_::region_exit`). A
/// counter is exactly the M2l shape: a missed pop prints nothing different for the
/// first 64 turns and then traps, and an extra pop reads as an enormous unsigned
/// depth on the very next `region`. So both directions are loud, but only if
/// something runs past 64 — and no example does. `region.vyrn` has two regions and
/// one `continue`-free loop; `controlflow.vyrn` has one `continue` under a region,
/// six turns.
///
/// Measured with each of the three unwind edges removed in turn: `break`,
/// `continue` and `return` each make this print nothing and trap where the
/// interpreter prints four numbers.
///
/// The nesting case is recursive rather than 65 literal blocks because the depth is
/// DYNAMIC — a callee's region nests inside its caller's, which is why the counter
/// is four bytes of memory and not a compile-time constant per body.
#[test]
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test parity -- --ignored"]
fn every_exit_out_of_a_region_balances_and_the_65th_traps() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = scratch("directwasm-region");
    for (what, src) in [
        (
            "balance",
            r#"
fn viaReturn(i: Int64) -> Int64 {
    region {
        if i >= 0 {
            return i + 1
        }
    }
    return 0
}

fn main() -> Int64 {
    // `continue` out of a region — controlflow.vyrn's shape, 200 turns.
    let mut acc = 0
    let mut j = 0
    while j < 200 {
        j = j + 1
        region {
            if j % 2 == 0 {
                continue
            }
            acc = acc + 1
        }
    }
    print(acc)

    // `break` out of a region, re-entered by an outer loop 200 times.
    let mut brk = 0
    let mut k = 0
    while k < 200 {
        k = k + 1
        let mut n = 0
        while n < 5 {
            region {
                if n == 2 {
                    break
                }
                brk = brk + 1
            }
            n = n + 1
        }
    }
    print(brk)

    // `return` out of a region, 200 calls.
    let mut r = 0
    let mut q = 0
    while q < 200 {
        r = r + viaReturn(q)
        q = q + 1
    }
    print(r)

    // Nested regions, fall-through exits only.
    let mut s = 0
    let mut t = 0
    while t < 200 {
        region {
            region {
                s = s + 1
            }
        }
        t = t + 1
    }
    print(s)
    return 0
}
"#,
        ),
        (
            "nested",
            r#"
fn deep(n: Int64) -> Int64 {
    if n == 0 {
        return 0
    }
    let mut r = 0
    region {
        r = deep(n - 1) + 1
    }
    return r
}

fn main() -> Int64 {
    print(deep(63))
    print(deep(70))
    return 0
}
"#,
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
            .output()
            .expect("build wasm");
        assert!(
            build.status.success(),
            "{what}: {}",
            String::from_utf8_lossy(&build.stderr)
        );

        let mut interp_cmd = vyrn();
        interp_cmd.arg("run").arg(&path);
        let interp = run_io(interp_cmd, &dir, &dir.join("no.stdin"));
        let mut wasm_cmd = Command::new(&wasmtime);
        wasm_cmd.arg("run").arg(&module);
        let w = run_io(wasm_cmd, &dir, &dir.join("no.stdin"));

        // The interpreter's own answers, not a spelling written here: two backends
        // can be confidently wrong about the depth bound together.
        assert_eq!(
            runtime_err(&interp.stderr),
            runtime_err(&w.stderr),
            "{what}: stderr"
        );
        assert_eq!(norm(&interp.stdout), norm(&w.stdout), "{what}: stdout");
        assert_eq!(interp.status.code(), w.status.code(), "{what}: exit");
        // The two cases have to be opposite, or a run in which nothing at all
        // happened would satisfy every assertion above.
        assert_eq!(
            runtime_err(&w.stderr).is_empty(),
            what == "balance",
            "{what}: wrong outcome entirely — {:?}",
            runtime_err(&w.stderr)
        );
    }
}

/// `lineAt`/`colAt` at the offsets no example asks for, and one row that says what
/// a column counts.
///
/// `examples/textbytes.vyrn` sweeps the interesting middle — a CRLF, an empty
/// line, past the end, and `éx` proving column 3 for the `x` — but two cases it
/// never reaches are the two whose lowering is a compare's SIGNEDNESS:
///
/// - **A negative offset.** The interpreter clamps with `.max(0)`; the native shim
///   does not clamp at all and gets the same answer because its `i < off` and
///   `i > 0` are signed. This backend takes the shim's route, so `i64.ge_s` versus
///   `i64.ge_u` is the whole difference between `1:1` and a walk over four
///   exabytes. RFC-0078's oracle sweeps to `-3` for exactly this reason and the
///   RFC-0077 ladder cannot, because no `.vyrn` in the corpus passes a negative
///   offset.
/// - **An empty buffer**, where every line start and the length are zero, and the
///   `off > len` clamp is the only thing between `colAt` and a read below the
///   allocation.
///
/// Plus a byte column on a line that is NOT the first, because `std/vyx.vyrn:165`
/// documents `colAt` as counting chars and RFC-0078 M4b(2) measured that it counts
/// bytes. Every row is compared against the interpreter AND spelled out: two
/// backends can be confidently wrong together, which is how M2m's non-ASCII `=~`
/// walk passed every example it had.
#[test]
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test parity -- --ignored"]
fn line_and_column_agree_with_the_interpreter_off_both_ends_of_the_buffer() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = scratch("directwasm-linecol");
    let src = "\
fn lc(b: Array<UInt8>, off: Int64) -> String {
    return lineAt(b, off).toString() + \":\" + colAt(b, off).toString()
}

fn main() -> Int64 {
    let b = bytes(\"ab\\ncd\")
    // Below zero, and further below zero.
    print(lc(b, 0 - 1))
    print(lc(b, 0 - 3))
    // Exactly at the length, which is one past the last byte.
    print(lc(b, 5))
    // The empty buffer, from below, at, and past its one valid offset.
    print(lc(bytes(\"\"), 0 - 1))
    print(lc(bytes(\"\"), 0))
    print(lc(bytes(\"\"), 5))
    // Nothing but newlines: every offset starts a line of its own.
    let n = bytes(\"\\n\\n\\n\")
    print(lc(n, 0))
    print(lc(n, 1))
    print(lc(n, 3))
    print(lc(n, 4))
    // A two-byte codepoint on the SECOND line: the `x` is column 3, not column 2.
    let u = bytes(\"\\u{e9}\\n\\u{fc}x\")
    print(lc(u, 4))
    print(lc(u, 5))
    return 0
}
";
    let path = dir.join("lc.vyrn");
    std::fs::write(&path, src).unwrap();
    let module = dir.join("lc.wasm");
    let build = vyrn()
        .arg("build")
        .arg(&path)
        .arg("--target")
        .arg("wasm")
        .arg("-o")
        .arg(&module)
        .output()
        .expect("build wasm");
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let mut interp_cmd = vyrn();
    interp_cmd.arg("run").arg(&path);
    let interp = run_io(interp_cmd, &dir, &dir.join("no.stdin"));
    let mut wasm_cmd = Command::new(&wasmtime);
    wasm_cmd.arg("run").arg(&module);
    let w = run_io(wasm_cmd, &dir, &dir.join("no.stdin"));

    assert_eq!(
        norm(&interp.stdout),
        "1:1\n1:1\n2:3\n1:1\n1:1\n1:1\n1:1\n2:1\n4:1\n4:1\n2:2\n2:3\n",
        "the interpreter moved"
    );
    assert_eq!(norm(&interp.stdout), norm(&w.stdout), "stdout");
    assert_eq!(
        runtime_err(&interp.stderr),
        runtime_err(&w.stderr),
        "stderr"
    );
    assert_eq!(interp.status.code(), w.status.code(), "exit");
}

/// A generic enum whose type argument comes from a `match` arm that is not the
/// FIRST one (RFC-0077 M2n).
///
/// `genericpayload.vyrn` is the corpus's only generic-payload example and it puts
/// the concrete arm first, so a first-arm-wins rule passes it. This is the order
/// that rule gets wrong — and the order the CHECKER only permits with an
/// annotation, which is why no example has it: without one it refuses `Empty` as
/// uninferable.
///
/// Two payload shapes, because they fail differently. A `Cargo` payload is
/// `Word::Boxed`, so forgetting `T` refuses (`a conversion from Cargo to Unit`,
/// which is exactly what this source produced with the arm scan removed). An
/// `Int64` payload is `Word::Direct`, so the SAME mistake has no conversion to
/// refuse — the word is an `i64` either way — and would read a pointer as a
/// number. So the values are pinned, not just the agreement.
#[test]
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test parity -- --ignored"]
fn a_generic_payload_is_typed_by_whichever_arm_knows_it() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = scratch("directwasm-payload");
    let src = r#"
type Cargo = { weight: Int64, label: String }
type Crate<T> = | Empty | Held(T)
type Choice = | First | Second

fn boxedPayload(p: Choice) -> Cargo {
    let boxed: Crate<Cargo> = match p {
        Second => Empty,
        First => Held(Cargo { weight: 3, label: "three" }),
    }
    return match consume boxed {
        Empty => Cargo { weight: 99, label: "fallback" },
        Held(s) => s,
    }
}

fn directPayload(p: Choice) -> Int64 {
    let boxed: Crate<Int64> = match p {
        Second => Empty,
        First => Held(41),
    }
    return match boxed {
        Empty => 0 - 1,
        Held(n) => n + 1,
    }
}

fn main() -> Int64 {
    let a = boxedPayload(First)
    print("boxed first: \{a.weight} \{a.label}")
    let b = boxedPayload(Second)
    print("boxed second: \{b.weight} \{b.label}")
    print("direct first: \{directPayload(First)}")
    print("direct second: \{directPayload(Second)}")
    return 0
}
"#;
    let path = dir.join("armorder.vyrn");
    std::fs::write(&path, src).unwrap();
    let module = dir.join("armorder.wasm");
    let build = vyrn()
        .arg("build")
        .arg(&path)
        .arg("--target")
        .arg("wasm")
        .arg("-o")
        .arg(&module)
        .output()
        .expect("build wasm");
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let mut interp_cmd = vyrn();
    interp_cmd.arg("run").arg(&path);
    let interp = run_io(interp_cmd, &dir, &dir.join("no.stdin"));
    let mut wasm_cmd = Command::new(&wasmtime);
    wasm_cmd.arg("run").arg(&module);
    let w = run_io(wasm_cmd, &dir, &dir.join("no.stdin"));

    let want = "boxed first: 3 three\nboxed second: 99 fallback\n\
                direct first: 42\ndirect second: -1\n";
    assert_eq!(norm(&interp.stdout), want, "the interpreter moved");
    assert_eq!(norm(&interp.stdout), norm(&w.stdout), "stdout");
    assert_eq!(
        runtime_err(&interp.stderr),
        runtime_err(&w.stderr),
        "stderr"
    );
    assert_eq!(interp.status.code(), w.status.code(), "exit");
}

/// RFC-0008's two sinks and three thresholds the corpus does not reach
/// (RFC-0077 M2n).
///
/// `logging.vyrn` and `vlog.vyrn` are the only two logging examples and both write
/// to `stderr`; `logging.vyrn` is the only one this backend can build at all, so
/// `stdout`, `file(..)`, and every threshold but `debug` have no example.
///
/// The three cases are chosen for what would go unnoticed:
///
/// - **`stdout`.** The log line and `print` go to the SAME descriptor, so their
///   interleaving is observable and a sink that quietly stayed on 2 would still
///   look right in a test that only read stdout.
/// - **`file(..)`.** A descriptor opened once and held, which `writeFile` cannot
///   express. Run TWICE, because `path_open` without `TRUNC` appends and one run
///   cannot tell the difference; the file's contents are compared against the
///   interpreter's own `std::fs::File`, not against a spelling here.
/// - **`level: error`.** Everything below the threshold is dropped, so the only
///   log line in the file is the last one — and the `\{n}` in a suppressed
///   message still runs, which is RFC-0008's Q4 and the half a fold could
///   silently take away.
#[test]
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test parity -- --ignored"]
fn a_log_sink_is_whichever_descriptor_the_config_named() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = scratch("directwasm-logsink");
    for (what, src, want_out, want_err, want_file) in [
        (
            "stdout",
            "logging { level: info, sink: stdout }\n\
             \n\
             fn side(n: Int64) -> Int64 {\n\
             \x20   print(\"side \\{n}\")\n\
             \x20   return n\n\
             }\n\
             \n\
             fn main() -> Int64 {\n\
             \x20   let log = logger(\"sink\")\n\
             \x20   log.debug(\"dropped \\{side(1)}\")\n\
             \x20   log.info(\"kept\")\n\
             \x20   print(\"program output\")\n\
             \x20   log.error(\"last\")\n\
             \x20   return 0\n\
             }\n",
            // `side(1)` prints even though the `debug` line does not: the
            // arguments of a suppressed call are still evaluated.
            "side 1\n[INFO] sink: kept\nprogram output\n[ERROR] sink: last\n",
            "",
            None,
        ),
        (
            "file",
            "logging { level: debug, sink: file(\"sink.log\") }\n\
             \n\
             fn main() -> Int64 {\n\
             \x20   let log = logger(\"sink\")\n\
             \x20   log.trace(\"dropped\")\n\
             \x20   log.debug(\"first\")\n\
             \x20   log.warn(\"second\")\n\
             \x20   print(\"program output\")\n\
             \x20   return 0\n\
             }\n",
            "program output\n",
            "",
            Some("[DEBUG] sink: first\n[WARN] sink: second\n"),
        ),
        (
            "threshold",
            "logging { level: error, sink: file(\"sink.log\") }\n\
             \n\
             fn main() -> Int64 {\n\
             \x20   let log = logger(\"sink\")\n\
             \x20   log.trace(\"a\")\n\
             \x20   log.debug(\"b\")\n\
             \x20   log.info(\"c\")\n\
             \x20   log.warn(\"d\")\n\
             \x20   log.error(\"e\")\n\
             \x20   return 0\n\
             }\n",
            "",
            "",
            Some("[ERROR] sink: e\n"),
        ),
    ] {
        // Its own directory, because the file sink names a relative path and two
        // cases writing one `sink.log` would read each other's run.
        let dir = dir.join(what);
        std::fs::create_dir_all(&dir).unwrap();
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
            .output()
            .expect("build wasm");
        assert!(
            build.status.success(),
            "{what}: {}",
            String::from_utf8_lossy(&build.stderr)
        );

        let log = dir.join("sink.log");
        let read_log = || {
            std::fs::read_to_string(&log)
                .unwrap_or_default()
                .replace("\r\n", "\n")
        };

        // Twice each, so a sink that APPENDS where the interpreter truncates is a
        // failure rather than a coincidence.
        let _ = std::fs::remove_file(&log);
        let mut interp_cmd = vyrn();
        interp_cmd.arg("run").arg(&path);
        run_io(interp_cmd, &dir, &dir.join("no.stdin"));
        let mut interp_cmd = vyrn();
        interp_cmd.arg("run").arg(&path);
        let interp = run_io(interp_cmd, &dir, &dir.join("no.stdin"));
        let interp_log = read_log();

        let _ = std::fs::remove_file(&log);
        let mut wasm_cmd = Command::new(&wasmtime);
        wasm_cmd.arg("run").arg("--dir").arg(".").arg(&module);
        run_io(wasm_cmd, &dir, &dir.join("no.stdin"));
        let mut wasm_cmd = Command::new(&wasmtime);
        wasm_cmd.arg("run").arg("--dir").arg(".").arg(&module);
        let w = run_io(wasm_cmd, &dir, &dir.join("no.stdin"));
        let wasm_log = read_log();

        assert_eq!(
            norm(&interp.stdout),
            want_out,
            "{what}: the interpreter moved (stdout)"
        );
        assert_eq!(
            runtime_err(&interp.stderr),
            want_err,
            "{what}: the interpreter moved (stderr)"
        );
        assert_eq!(
            interp_log,
            want_file.unwrap_or("").to_string(),
            "{what}: the interpreter moved (file)"
        );
        assert_eq!(norm(&interp.stdout), norm(&w.stdout), "{what}: stdout");
        assert_eq!(
            runtime_err(&interp.stderr),
            runtime_err(&w.stderr),
            "{what}: stderr"
        );
        assert_eq!(interp_log, wasm_log, "{what}: the log file");
        assert_eq!(interp.status.code(), w.status.code(), "{what}: exit");
    }
}

/// A suppressed log call emits **no write**, and the evidence is in the bytes
/// (RFC-0077 M2n).
///
/// The threshold fold is the one part of RFC-0008 that a passing ladder cannot
/// vouch for: a backend that emitted a runtime comparison instead would print the
/// same lines and pass every assertion above. So this reads the module.
///
/// `[LEVEL] ` is interned at the emitting site and nowhere else, so its presence
/// means a write exists and its absence means one does not — the same argument M2d
/// makes about a validation's trap message. The suppressed call's own MESSAGE is
/// asserted present, because its arguments are still evaluated (Q4): a fold that
/// deleted the whole statement would pass a test that only looked for the prefix.
#[test]
fn a_suppressed_log_call_is_not_in_the_module() {
    let dir = scratch("directwasm-logfold");
    // `warn`, so two levels below it and two at-or-above are in one program.
    let src = "logging { level: warn, sink: stderr }\n\
               \n\
               fn main() -> Int64 {\n\
               \x20   let log = logger(\"f\")\n\
               \x20   log.trace(\"gone-trace\")\n\
               \x20   log.debug(\"gone-debug\")\n\
               \x20   log.info(\"gone-info\")\n\
               \x20   log.warn(\"kept-warn\")\n\
               \x20   log.error(\"kept-error\")\n\
               \x20   return 0\n\
               }\n";
    let path = dir.join("fold.vyrn");
    std::fs::write(&path, src).unwrap();
    let module = dir.join("fold.wasm");
    let build = vyrn()
        .arg("build")
        .arg(&path)
        .arg("--target")
        .arg("wasm")
        .arg("-o")
        .arg(&module)
        .output()
        .expect("build wasm");
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let bytes = std::fs::read(&module).unwrap();
    let has = |needle: &str| bytes.windows(needle.len()).any(|w| w == needle.as_bytes());
    for lvl in ["[TRACE] ", "[DEBUG] ", "[INFO] "] {
        assert!(
            !has(lvl),
            "{lvl} is in the module, so a suppressed call emitted a write"
        );
    }
    for lvl in ["[WARN] ", "[ERROR] "] {
        assert!(
            has(lvl),
            "{lvl} is NOT in the module, so an enabled call emitted nothing"
        );
    }
    // The other half of Q4: the suppressed calls' arguments are still evaluated,
    // so their strings are still there. Without this the test would also pass on a
    // backend that dropped the statement whole.
    for msg in [
        "gone-trace",
        "gone-debug",
        "gone-info",
        "kept-warn",
        "kept-error",
    ] {
        assert!(
            has(msg),
            "`{msg}` is not in the module: a suppressed call lost its argument"
        );
    }
}

/// RFC-0012's two host boundaries, which nothing but a browser can drive.
///
/// `externdemo.vyrn` is in `WASM_ONLY` precisely because wasmtime supplies WASI and
/// not `vyrn`, so there has never been a run to compare — which is how this backend
/// reached 87/87 having never lowered an `extern` **import** at all, and how nobody
/// noticed it named no exports but `_start` either (`--export-all` was doing that on
/// the LLVM path). Both were found by loading `web/externdemo.html` and
/// `web/domdemo.html`, and the ABI *shapes* stay verified there — a `(ptr, len)` pair
/// only means something to a host that decodes it.
///
/// What is pinned here is the half that was simply ABSENT, and it is pinned on the
/// module's bytes for M2o's reason (a name only one emit site writes is proof that
/// site ran):
///
/// - the import exists under the `vyrn` namespace, name for name — the length-
///   prefixed pair is exact, so this cannot pass on a module that merely mentions
///   the word somewhere;
/// - `__vyrn_malloc` is exported under exactly the condition that needs it. A JS
///   caller cannot pass a `String` INTO an export without allocating inside the
///   module first, and every `vyrn-dom.js` handler takes one — so a missing export
///   is not a missing feature, it is a demo where no button works. The negative
///   case is the assertion that matters: the condition is the thing that could be
///   wrong, and always exporting it would pass a one-sided test.
#[test]
fn the_rfc_0012_host_boundary_is_named_in_the_module() {
    let dir = examples_dir();
    let out = scratch("directwasm-extern");
    let build = |name: &str| -> Vec<u8> {
        let module = out.join(format!("{name}.wasm"));
        let b = vyrn()
            .arg("build")
            .arg(dir.join(format!("{name}.vyrn")))
            .arg("--target")
            .arg("wasm")
            .arg("-o")
            .arg(&module)
            .output()
            .expect("build wasm");
        assert!(b.status.success(), "{name}: {}", norm(&b.stderr));
        std::fs::read(&module).unwrap()
    };
    let has = |bytes: &[u8], needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);

    // An import entry is `<mod len><mod><field len><field>`, so the namespace and
    // the name are one literal and cannot be satisfied separately.
    let externdemo = build("externdemo");
    for field in ["jsLog", "jsNow", "jsAdd"] {
        let mut needle = vec![4u8];
        needle.extend_from_slice(b"vyrn");
        needle.push(field.len() as u8);
        needle.extend_from_slice(field.as_bytes());
        assert!(
            has(&externdemo, &needle),
            "externdemo does not import `vyrn.{field}` — the page has nothing to supply"
        );
    }

    // `greet(name: String)`, so the allocator is reachable; `onTick()`/`reset()`
    // take nothing, so it is not.
    assert!(
        has(&build("externdemo2"), b"__vyrn_malloc"),
        "a String parameter on an `export extern fn` needs the module's allocator exported"
    );
    assert!(
        !has(&build("eventloop"), b"__vyrn_malloc"),
        "the allocator is exported by a module with no String-taking export"
    );
}

/// Run one ad-hoc source under every engine and return `(interp, native, wasm)`
/// as `(stdout, stderr, exit)` triples, normalized the way the corpus loop above
/// normalizes.
///
/// The three pins that follow are all NATIVE defects, which the pins written for
/// RFC-0077 M2 could not have caught: those compared the interpreter against the
/// direct wasm backend, and on each of these three the two of them AGREED and
/// native was alone. So this helper exists rather than a fourth copy of the
/// build-and-compare block, and the wasm column comes along because it is free
/// and because a pin that names only two engines is how a third drifts.
#[allow(clippy::type_complexity)]
fn three_engines(
    tag: &str,
    what: &str,
    src: &str,
) -> Vec<(&'static str, String, String, Option<i32>)> {
    three_engines_in(&scratch(&format!("parity-{tag}")), what, src)
}

/// [`three_engines`] over a directory the caller already has — for the one pin
/// whose program IMPORTS a second file, which has to be written beside it. Each
/// scratch directory is now this process's alone, so "the same tag twice" is no
/// longer a way to share one.
#[allow(clippy::type_complexity)]
fn three_engines_in(
    dir: &Path,
    what: &str,
    src: &str,
) -> Vec<(&'static str, String, String, Option<i32>)> {
    let path = dir.join(format!("{what}.vyrn"));
    std::fs::write(&path, src).unwrap();
    let no_stdin = dir.join("no.stdin");

    let mut out = Vec::new();
    let mut interp_cmd = vyrn();
    interp_cmd.arg("run").arg(&path);
    let i = run_io(interp_cmd, &dir, &no_stdin);
    out.push((
        "interp",
        norm(&i.stdout),
        runtime_err(&i.stderr),
        i.status.code(),
    ));

    let exe = dir.join(format!("{what}.exe"));
    let b = vyrn()
        .arg("build")
        .arg(&path)
        .arg("-o")
        .arg(&exe)
        .output()
        .expect("build native");
    assert!(
        b.status.success(),
        "{what}: NATIVE BUILD FAILED\n{}{}",
        norm(&b.stdout),
        norm(&b.stderr)
    );
    let mut n_cmd = Command::new(&exe);
    n_cmd.env("VYRN_FREE_AUDIT", "1");
    let n = run_io(n_cmd, &dir, &no_stdin);
    out.push((
        "native",
        norm(&n.stdout),
        runtime_err(&n.stderr),
        n.status.code(),
    ));

    if let Some(wasmtime) = wasmtime() {
        let module = dir.join(format!("{what}.wasm"));
        let b = vyrn()
            .arg("build")
            .arg(&path)
            .arg("--target")
            .arg("wasm")
            .arg("-o")
            .arg(&module)
            .output()
            .expect("build wasm");
        assert!(
            b.status.success(),
            "{what}: wasm build: {}",
            norm(&b.stderr)
        );
        let mut c = Command::new(&wasmtime);
        c.arg("run").arg(&module);
        let w = run_io(c, &dir, &no_stdin);
        out.push((
            "wasm",
            norm(&w.stdout),
            runtime_err(&w.stderr),
            w.status.code(),
        ));
    }
    out
}

/// Assert every engine agrees with the INTERPRETER, and that the interpreter said
/// what is expected — two backends can be confidently wrong together, and on all
/// three of the defects below exactly two were.
fn all_agree(rows: &[(&str, String, String, Option<i32>)], what: &str) {
    let (_, out, err, code) = &rows[0];
    assert!(
        !out.is_empty() || !err.is_empty(),
        "{what}: no engine printed anything"
    );
    for (eng, o, e, c) in &rows[1..] {
        assert_eq!(o, out, "{what}: {eng} stdout");
        assert_eq!(e, err, "{what}: {eng} stderr");
        assert_eq!(c, code, "{what}: {eng} exit");
    }
}

/// `NaN != NaN` is TRUE, and native was the one engine that said otherwise.
///
/// IEEE 754 makes `!=` the UNORDERED comparison — the only one of the six whose
/// answer is `true` when an operand is a NaN — and the interpreter (Rust's `f64`
/// `!=`) and the direct wasm backend (`f64.ne`) both implement it. The textual
/// emitter spelled it `fcmp one`, "ordered AND not equal", which is `false` for a
/// NaN operand: `nan != nan` printed `1` under `vyrn run` and under wasmtime and
/// `0` natively.
///
/// The other five arms are here because the same class could have hidden in any of
/// them and no example compares against a NaN, so nothing would have said. They
/// are all ordered on all three engines and all print `0` — which is what makes
/// the `!=` rows load-bearing: they are the only two that are not `0`.
///
/// `zero / zero` rather than a NaN literal: the language has no NaN literal, and a
/// runtime division is also what stops `consteval` folding the comparison away and
/// answering with the compiler's arithmetic instead of the backend's.
#[test]
#[ignore = "needs clang; run explicitly: cargo test -p vyrn-cli --test parity -- --ignored"]
fn a_nan_is_not_equal_to_itself_on_every_engine() {
    let rows = three_engines(
        "nan",
        "nancmp",
        r#"
fn main() -> Int64 {
    let zero = 0.0
    let nan = zero / zero
    let one = 1.0
    print(if nan != nan { 1 } else { 0 })
    print(if nan == nan { 1 } else { 0 })
    print(if nan < one { 1 } else { 0 })
    print(if nan <= one { 1 } else { 0 })
    print(if nan > one { 1 } else { 0 })
    print(if nan >= one { 1 } else { 0 })
    print(if one != nan { 1 } else { 0 })
    // A NaN comparison must not poison an ordinary one, and `!=` on two ordinary
    // floats must still be `!=`.
    print(if one != 2.0 { 1 } else { 0 })
    print(if one != 1.0 { 1 } else { 0 })
    return 0
}
"#,
    );
    all_agree(&rows, "nancmp");
    // Spelled out as well as compared, because "all three engines say 0" is what
    // the bug looked like: the interpreter is the reference, so its answer is
    // asserted against IEEE 754 rather than against the other two.
    assert_eq!(
        rows[0].1, "1\n0\n0\n0\n0\n0\n1\n1\n0\n",
        "IEEE 754: only `!=` is unordered"
    );
}

/// A contextual array literal is built at the element type its slot DECLARES.
///
/// Inferring it from `elems[0]` instead made a bare integer literal an `Int`, so
/// `Array<UInt8> = [65, 66]` emitted a `[2 x i64]` aggregate and the consumer then
/// read it at the declared width. Three separate failures, and only the first was
/// loud:
///
/// - `Array<T>` stored the aggregate into the `{ ptr, i64, i64 }` triple — a clang
///   error, so native did not build while `vyrn run` and wasm printed `65`.
/// - `SmallArray<T, N>` did `extractvalue [2 x i8]` off it — the same clang error,
///   found by looking rather than by a report.
/// - `Array<Age>` (RFC-0020) went SILENT instead: the `ArrayN -> Array` step
///   reshapes whenever `llt` matches, `Age`'s `llt` IS `i64`, so no `where`
///   predicate ran at all. `[20, 5]` into an `Array<Age>` trapped under the
///   interpreter and under wasm and printed `20` and `5` natively.
///
/// `Array<Int64>` and an empty literal plus `push` always worked, which is why the
/// corpus had nothing: `examples/textbytes.vyrn` carried a `buf(Array<Int64>) ->
/// Array<UInt8>` helper for exactly this, documented as a workaround, and it is
/// deleted now — its malformed table is spelled with byte literals in `main`.
#[test]
#[ignore = "needs clang; run explicitly: cargo test -p vyrn-cli --test parity -- --ignored"]
fn a_sized_integer_array_literal_is_built_at_its_declared_width() {
    let rows = three_engines(
        "sizedlit",
        "widths",
        r#"
fn take(b: Array<UInt8>) -> Int64 {
    let mut s = 0
    for x in b {
        s = s + Int64(x)
    }
    return s
}

/// A RUNTIME wrap, so the truncation is the backend's and not `consteval`'s. Two
/// things the checker settles rather than this backend, both found by writing the
/// row: an out-of-range LITERAL element is a compile error (`300` is not a `UInt8`),
/// and a bare `Int64` element is not one either — `[n, n + 1]` is rejected with
/// "array elements must share a type", so the conversion is written out.
fn wrap(n: Int64) -> Array<UInt8> {
    return [UInt8(n), UInt8(n + 1)]
}

fn main() -> Int64 {
    // The reported shape: a `let` annotation.
    let b: Array<UInt8> = [65, 66]
    print(Int64(b[0]))
    print(Int64(b[1]))
    print(b.length)
    // Sized-int elements narrower AND wider than a byte, signed and not. Each
    // element is a bare literal, which is the whole bug: it inferred `Int`.
    let i32s: Array<Int32> = [1, 2, 3]
    print(Int64(i32s[1]))
    let i8s: Array<Int8> = [127, 1]
    print(Int64(i8s[0]) + Int64(i8s[1]))
    let u16s: Array<UInt16> = [65535, 1]
    print(Int64(u16s[0]))
    // The wrap a sized slot performs is the wrap the interpreter performs.
    let wrapped = wrap(300)
    print(Int64(wrapped[0]))
    print(Int64(wrapped[1]))
    // An ARGUMENT position, not just a `let`.
    print(take([1, 2, 3]))
    // A `SmallArray` slot (RFC-0056), whose lowering read the same aggregate at
    // the declared width from a different place.
    let sa: SmallArray<UInt8, 4> = [65, 66]
    print(Int64(sa[0]) + Int64(sa[1]))
    print(sa.length)
    // `Array<Int64>` and empty-plus-push always worked; here so a regression in
    // the path that DID work is caught by the same test.
    let plain: Array<Int64> = [7, 8]
    print(plain[0] + plain[1])
    let mut grown: Array<UInt8> = []
    grown.push(9)
    print(Int64(grown[0]))
    // Nested growable elements — the one case the old code got right, because it
    // was the one case that used the declared element type.
    let nested: Array<Array<Int64>> = [[1], [2, 3]]
    print(nested[1][1])
    return 0
}
"#,
    );
    all_agree(&rows, "widths");
    assert_eq!(
        rows[0].1, "65\n66\n2\n2\n128\n65535\n44\n45\n6\n131\n2\n15\n9\n3\n",
        "the interpreter's widths"
    );

    // The silent half: a validated element type, where the reshape skipped the
    // predicate because the representation matched. Its own program because the
    // expected outcome is a TRAP, and a trap ends the run.
    let rows = three_engines(
        "sizedlit",
        "validated",
        r#"
type Age = Int64 where value >= 18

fn mkAges(a: Int64, b: Int64) -> Array<Age> {
    return [a, b]
}

fn main() -> Int64 {
    let ok = mkAges(20, 30)
    print(ok[0] + ok[1])
    let bad = mkAges(20, 5)
    print(bad[0])
    return 0
}
"#,
    );
    all_agree(&rows, "validated");
    assert_eq!(rows[0].1, "50\n", "the valid pair prints before the trap");
    assert_eq!(
        rows[0].2, "error: validation failed for `Age`\n",
        "5 is not an `Age`"
    );
    assert_eq!(rows[0].3, Some(1), "a failed validation exits 1");
}

/// A rendered `Bool` owns its storage, on every engine.
///
/// `@str` of a `Bool` handed back the interned `"true"`/`"false"` on the direct
/// backend and a copy of it on the other two. The pointer alone is not the defect
/// — the ownership behind it is: an accumulator seeded with one takes
/// `str_append`'s ours-branch, reads the literal's `cap` (all ones, the sentinel
/// that says "static"), decides it never has to grow, and writes the appended
/// bytes and a new length straight into the data segment.
///
/// The program is the smallest shape that shows it: `s` is only ever self-appended
/// and read for its length, which is what makes it an eligible in-place
/// accumulator (`append_candidates`), and a SECOND interpolation of the same
/// `Bool` afterwards is what reads the literal the first one overwrote. Before the
/// fix wasm printed `20`, `20`, `true0123456789abcdef` where both other engines
/// printed `20`, `4`, `true` — the length written over `"true"`'s own header, and
/// the sixteen bytes written over its NUL and past it.
#[test]
#[ignore = "needs clang; run explicitly: cargo test -p vyrn-cli --test parity -- --ignored"]
fn a_rendered_bool_owns_its_buffer_on_every_engine() {
    let rows = three_engines(
        "boolstr",
        "boolrender",
        r#"
fn main() -> Int64 {
    let flag = true
    let mut s = "\{flag}"
    s = s + "0123456789abcdef"
    let n = s.byteLength
    let again = "\{flag}"
    print(n)
    print(again.byteLength)
    print(again)
    return 0
}
"#,
    );
    all_agree(&rows, "boolrender");
    assert_eq!(
        rows[0].1, "20\n4\ntrue\n",
        "the literal is untouched by what was rendered out of it"
    );
}

/// Every edge that leaves a `region` balances the region stack NATIVELY, taken
/// more often than the stack is deep.
///
/// RFC-0077 M2m's `every_exit_out_of_a_region_balances_and_the_65th_traps` is this
/// test's shape and it measured the direct backend; this one measures the textual
/// one, where `return` popped nothing at all. `Stmt::Region` emitted
/// `@__vyrn_region_exit` on the fall-through path only, so a function returning out
/// of a region consumed a slot per call: 70 calls printed `4900` under the
/// interpreter and `error: region nesting exceeds 64`, exit 1, natively.
///
/// The fix is a pop that does NOT free (`@__vyrn_region_pop`), and the reason is in
/// `REGION_RUNTIME`: `return a + b` can hand back a pointer into the frame it is
/// leaving and RFC-0004's escape guard examines stores into named bindings, not
/// return values. So reclamation on the return edge is deferred rather than
/// attempted — it needs an escape analysis that does not exist, and this backend
/// already frees nothing for `push` or for `Stmt::Drop`.
///
/// `break` and `continue` were checked for the same hole and do not have it:
/// `emit_loop_exit_cleanup` has unwound the regions opened inside a loop body since
/// RFC-0060, and it can keep FREEING because `region_store_guard` does cover the
/// stores those edges can make. `?` propagation did have it, and is covered here
/// because it shares `emit_all_drops` with `return`.
///
/// Measured by sabotage, M2m's method: with the pop removed from `emit_all_drops`
/// this traps at `error: region nesting exceeds 64` and prints nothing after the
/// first two numbers, where the interpreter prints six.
#[test]
#[ignore = "needs clang; run explicitly: cargo test -p vyrn-cli --test parity -- --ignored"]
fn a_return_out_of_a_region_balances_the_region_stack_on_every_engine() {
    let rows = three_engines(
        "regionret",
        "balance",
        r#"
fn viaReturn(n: Int64) -> Int64 {
    region {
        let a = n * 2
        return a + 1
    }
    return 0
}

/// A region left by `return` from INSIDE a loop inside the region, so the return
/// unwinds a loop and a region together.
fn viaLoop(n: Int64) -> Int64 {
    region {
        let mut i = 0
        while i < 10 {
            if i == n % 10 {
                return i
            }
            i = i + 1
        }
    }
    return -1
}

/// Two regions open at the return, so one pop would not be enough.
fn viaNested(n: Int64) -> Int64 {
    region {
        region {
            return n + 1
        }
    }
    return 0
}

/// `?` propagation out of a region: the same early-return edge, reached by an
/// operator rather than by the keyword.
fn viaTry(n: Int64) -> Option<Int64> {
    region {
        let half = if n % 2 == 0 { Some(n / 2) } else { None }
        let h = half?
        return Some(h + 1)
    }
    return None
}

/// A String built inside the region and returned out of it — the case that forbids
/// the free. It must print, not crash.
fn viaString(n: Int64) -> String {
    region {
        return "n=" + n.toString()
    }
    return ""
}

fn main() -> Int64 {
    let mut a = 0
    let mut i = 0
    while i < 70 {
        a = a + viaReturn(i)
        i = i + 1
    }
    print(a)

    let mut b = 0
    let mut j = 0
    while j < 200 {
        b = b + viaLoop(j)
        j = j + 1
    }
    print(b)

    let mut c = 0
    let mut k = 0
    while k < 200 {
        c = c + viaNested(k)
        k = k + 1
    }
    print(c)

    let mut d = 0
    let mut m = 0
    while m < 200 {
        d = d + match viaTry(m) {
            Some(v) => v.copy(),
            None => 0,
        }
        m = m + 1
    }
    print(d)

    let mut last = ""
    let mut p = 0
    while p < 200 {
        last = viaString(p)
        p = p + 1
    }
    print(last)

    // `break` and `continue` out of a region, 200 turns each — the edges that
    // already unwound, so a regression there fails here too.
    let mut e = 0
    let mut q = 0
    while q < 200 {
        q = q + 1
        region {
            if q % 2 == 0 {
                continue
            }
            e = e + 1
        }
    }
    print(e)

    let mut f = 0
    let mut r = 0
    while r < 200 {
        r = r + 1
        let mut n = 0
        while n < 5 {
            region {
                if n == 2 {
                    break
                }
                f = f + 1
            }
            n = n + 1
        }
    }
    print(f)
    return 0
}
"#,
    );
    all_agree(&rows, "balance");
    // The interpreter's own numbers, and every one of them requires the run to
    // have got past 64 regions: 4900 is 70 returns, and the rest are 200 turns.
    assert_eq!(
        rows[0].1, "4900\n900\n20100\n5050\nn=199\n100\n400\n",
        "the balanced answers"
    );
    assert_eq!(rows[0].2, "", "nothing traps once the stack balances");
    assert_eq!(rows[0].3, Some(0), "exit 0");

    // The depth bound itself still refuses at the same place, so the pop did not
    // just disable the check. Recursive, because the depth is dynamic.
    let rows = three_engines(
        "regionret",
        "deep",
        r#"
fn deep(n: Int64) -> Int64 {
    if n == 0 {
        return 0
    }
    let mut r = 0
    region {
        r = deep(n - 1) + 1
    }
    return r
}

fn main() -> Int64 {
    print(deep(63))
    print(deep(70))
    return 0
}
"#,
    );
    all_agree(&rows, "deep");
    assert_eq!(rows[0].1, "63\n", "63 nested regions are fine");
    assert_eq!(
        rows[0].2, "error: region nesting exceeds 64\n",
        "the 65th is not"
    );
    assert_eq!(rows[0].3, Some(1));
}

/// `panic(msg)` (RFC-0079 M1) — the first runtime message whose TEXT a program
/// wrote, and the reason RFC-0078 had refused a user-callable abort.
///
/// The objection was that parity compares stderr byte for byte and
/// library-authored text is text the compiler no longer single-sources. What is
/// actually single-sourced is the FRAME — `error: `, the newline, exit 1 — and
/// each engine assembles it differently: the interpreter hands the message to the
/// same `Ctrl::Err` channel every `@.trap.*` uses and the CLI prefixes it, the
/// textual backend `fprintf`s one format, and the direct backend writes three
/// pieces and hands the last to `trap`. Three assemblies of one line is exactly
/// the shape that drifts, so the message here carries a **non-ASCII byte** and an
/// **interpolation**: a length taken in characters rather than bytes truncates
/// `«bäd»`, and an argument evaluated after the prefix is written reorders it.
///
/// The other two cases are about `Never` rather than about the bytes.
///
/// A `panic` **in a match arm** is the unification that matters — the arm has no
/// type and the other arm decides, which every join in both backends had to be
/// taught. It is the shape M3's `?? panic("..")` desugars into, so `slice`
/// becoming Vyrn rests on it.
///
/// A `panic` **inside a region** is here for the class 911efb2 fixed: an exit out
/// of a region owes the region stack an unwind, and a lowering that forgot one
/// left the counter raised. `panic` owes NOTHING — the process is gone before the
/// next allocation — and the way to tell "correctly owes nothing" from "forgot"
/// is that the message still arrives, in full, from two regions deep.
#[test]
#[ignore = "needs clang; run explicitly: cargo test -p vyrn-cli --test parity -- --ignored"]
fn a_panic_says_the_same_bytes_on_all_three_engines() {
    // The message: a `\{}` hole, and `ø`/`«»`/`ä` outside ASCII.
    let rows = three_engines(
        "panic",
        "bytes",
        r#"
fn label(x: Option<Int64>, tag: String) -> String {
    return match x {
        Some(n) => "n=\{n}",
        None => panic("wrøng tag «\{tag}» — nothing to label"),
    }
}

fn main() -> Int64 {
    print(label(Some(7), "ok"))
    region {
        let inside = "région"
        print(inside)
        print(label(None, "bäd"))
    }
    return 0
}
"#,
    );
    all_agree(&rows, "bytes");
    // Spelled out, not only compared: the failure this is about is a message that
    // still looks like a message — one byte short, or the prefix in the wrong
    // place — and three engines can be wrong together about where `error: ` goes.
    assert_eq!(
        rows[0].1, "n=7\nrégion\n",
        "the live arm ran, and the region printed"
    );
    assert_eq!(
        rows[0].2, "error: wrøng tag «bäd» — nothing to label (bytes.vyrn:5)\n",
        "the caller's text, framed by the compiler, with the site the loader stamped"
    );
    assert_eq!(rows[0].3, Some(1), "exit 1, like every trap");

    // Two regions deep, as a bare statement rather than through a call: the
    // region stack is at depth 2 and the arena holds `hëld` when the process ends.
    let rows = three_engines(
        "panic",
        "region",
        r#"
fn main() -> Int64 {
    let mut n = 0
    region {
        let held = "hëld"
        n = n + 1
        region {
            print(held)
            panic("inside two regions, \{n} deep")
        }
    }
    return 0
}
"#,
    );
    all_agree(&rows, "region");
    assert_eq!(
        rows[0].1, "hëld\n",
        "the region's own String survived to be printed"
    );
    assert_eq!(
        rows[0].2, "error: inside two regions, 1 deep (region.vyrn:9)\n",
        "the message, in full"
    );
    assert_eq!(rows[0].3, Some(1));

    // Every shape `Never` has to flow through, with the panic NOT taken — so what
    // is checked is that the OTHER arm's value arrives intact. A scalar join, an
    // aggregate one (the direct backend allocates the destination before either
    // arm runs, from a type a `Never` arm cannot name), a user enum's `switch`, an
    // `if` in both arm positions, and a function whose whole body is a `panic`.
    let rows = three_engines(
        "panic",
        "never",
        r#"
type Point = { x: Int64, y: Int64 }

type Shape =
    | Dot
    | Line(Int64)

fn firstArm(x: Option<Int64>) -> Int64 {
    return match x {
        Some(n) => panic("no"),
        None => 5,
    }
}

fn aggArm(x: Option<Int64>) -> Point {
    return match x {
        Some(n) => Point { x: n, y: n },
        None => panic("no point"),
    }
}

fn onEnum(s: Shape) -> Int64 {
    return match s {
        Dot => panic("a dot has no length"),
        Line(n) => n,
    }
}

fn onlyPanics(n: Int64) -> Int64 {
    panic("this function never returns \{n}")
}

fn main() -> Int64 {
    print(firstArm(None))
    let p = aggArm(Some(3))
    print(p.x + p.y)
    print(onEnum(Line(9)))
    print(if true { "then-wins" } else { panic("dead") })
    print(if false { panic("nope") } else { "else-wins" })
    region {
        if false { panic("not here") }
        print("region intact")
    }
    return 0
}
"#,
    );
    all_agree(&rows, "never");
    assert_eq!(
        rows[0].1, "5\n6\n9\nthen-wins\nelse-wins\nregion intact\n",
        "every join took the arm that has a value"
    );
    assert_eq!(rows[0].2, "", "nothing panicked");
    assert_eq!(rows[0].3, Some(0), "exit 0");
}

/// A `panic` in a LIBRARY reports the library, on all three engines (census U5).
///
/// This is the decision the census entry rested on, so it is pinned rather than
/// described. `c[9]` is written in `site.vyrn`; the refusal is written in
/// `bank.vyrn`, inside a `place at` projection — which RFC-0091 M2 inlines INTO
/// the access site, so at the moment each engine emits the trap the caller's
/// line is the one in hand. The location says `bank.vyrn:10` anyway.
///
/// The reason is uniformity. A `panic` inside an ordinary library function is
/// not inlined and has no access site to name, so a caller-line rule would apply
/// to projections and to nothing else: one construct, two meanings, decided by
/// whether the callee happened to be a projection. The caller's line is
/// reachable — `project::inline` already receives it — and is not taken.
///
/// The file name is derived from the project's shape, never from the resolved
/// key: `bank.vyrn`, not the absolute path this harness builds under. That is
/// what lets a wasm module carry a location at all — it has no filesystem at run
/// time, so the only path it can have is the one baked at compile time, and a
/// baked absolute path would differ per build machine.
#[test]
#[ignore = "needs clang; run explicitly: cargo test -p vyrn-cli --test parity -- --ignored"]
fn a_panic_in_a_library_names_the_library() {
    let dir = scratch("parity-panicsite");
    std::fs::write(
        dir.join("bank.vyrn"),
        r#"export type Cage = { xs: Array<Int64> }

export fn newCage() -> Cage {
    return Cage { xs: [1, 2] }
}

impl Index for Cage {
    fn at(read self, k: Int64) -> read Int64 {
        if k < 0 || k >= self.xs.length {
            panic("cage: no such key")
        }
        return self.xs[k]
    }
}
"#,
    )
    .unwrap();
    let rows = three_engines_in(
        &dir,
        "site",
        r#"import { Cage, newCage } from "./bank"

fn main() -> Int64 {
    let c = newCage()
    print(c[1])
    print(c[9])
    return 0
}
"#,
    );
    all_agree(&rows, "panicsite");
    assert_eq!(
        rows[0].1, "2\n",
        "the live access answered before the dead one refused"
    );
    assert_eq!(
        rows[0].2, "error: cage: no such key (bank.vyrn:10)\n",
        "the projection's own file and line, not the access site's"
    );
    assert_eq!(rows[0].3, Some(1), "exit 1, like every trap");
}

/// `??` on both sums, on all three engines (RFC-0079 M2).
///
/// `??` desugars in the parser to a `match` over two type-agnostic patterns, so
/// what this pins is not an operator — it is that the desugar's `Success`/
/// `Failure` pair resolves to the SAME tag on all three engines. Each of them
/// reads the tag its own way (an enum arm in the interpreter, an `i1` in the
/// textual backend, a one-byte load in the direct one), and a pair that agreed
/// on `Option` while disagreeing on `Result` would be silent in two columns out
/// of three.
///
/// The last line is the composition the whole RFC is for: `x ?? panic("…")` is
/// `unwrap`, spelled by the person who knows why, with no primitive of its own.
/// It runs LAST because it ends the process — everything above it has already
/// printed by then, which is what makes the stdout assertion load-bearing.
#[test]
#[ignore = "needs clang; run explicitly: cargo test -p vyrn-cli --test parity -- --ignored"]
fn nullish_and_panic_say_the_same_bytes_on_all_three_engines() {
    let rows = three_engines(
        "nullish",
        "both",
        r#"
fn half(n: Int64) -> Option<Int64> {
    if n % 2 == 0 {
        return Some(n / 2)
    }
    return None
}

fn toNum(s: String) -> Result<Int64, String> {
    if s == "one" {
        return Ok(1)
    }
    return Err("bad «\{s}»")
}

fn main() -> Int64 {
    print(half(10) ?? -1)
    print(half(7) ?? -1)
    print(toNum("one") ?? -1)
    print(toNum("två") ?? -1)
    print(half(7) ?? half(4) ?? -1)
    print(half(8) ?? panic("not taken"))
    print(toNum("one") ?? panic("not taken either"))
    print(toNum("tvä") ?? panic("no number in «tvä»"))
    return 0
}
"#,
    );
    // The wasm column is OPTIONAL in `three_engines` — it is skipped when
    // `wasmtime()` finds nothing, and a green run that never built a module has
    // fooled a reader before. This case says so out loud.
    assert_eq!(
        rows.len(),
        3,
        "wasmtime did not resolve, so wasm was never tested: {:?}",
        rows.iter().map(|r| r.0).collect::<Vec<_>>()
    );
    all_agree(&rows, "both");
    assert_eq!(
        rows[0].1, "5\n-1\n1\n-1\n2\n4\n1\n",
        "both sums unwrap, the chain is right-associative, and an untaken `panic` costs nothing"
    );
    // The discarded `Err` payload — a fresh interpolated String on each failing
    // call — appears on NEITHER channel. `Failure`'s binder exists so the payload
    // is bound and goes nowhere, not so it can be read.
    assert!(
        !rows[0].1.contains("bad «"),
        "an error payload reached stdout"
    );
    assert_eq!(
        rows[0].2, "error: no number in «tvä» (both.vyrn:24)\n",
        "the only text on stderr is the reason the caller wrote, and where they wrote it"
    );
    assert_eq!(rows[0].3, Some(1), "exit 1, like every trap");
}

/// A `String` accumulator grown IN PLACE says the same bytes as one copied
/// (RFC-0081).
///
/// `d4d96aa` gave the textual backend the append that `return out + "]"` had been
/// banning, and pinned the rule where the rule lives — `binop_retains_str` and
/// `append_candidates` are one whitelist in `vyrn-codegen`, and the direct wasm
/// backend now calls the SAME two functions rather than restating them. So what is
/// left to pin here is not the policy, it is the half each backend owns: whether
/// its own lowering of the whitelisted shape still computes the same string.
///
/// The four functions are the four ways the shadow `(len, cap)` beside the
/// accumulator can go stale, which is the only way an in-place append can be
/// wrong:
///
/// - `build` — the ordinary loop, plus the tail concat that used to disqualify it.
/// - `reassigned` — a whole-value store between appends. The pointer is a data
///   segment literal afterwards, so `cap` must go back to 0 or the next append
///   writes into read-only-by-convention bytes it never allocated.
/// - `perTurn` — a `let` INSIDE the loop, so the accumulator is a fresh unowned
///   pointer every turn while the wasm local and its frame slot are the same two
///   words. The second turn is the one that catches a shadow that is not reset.
/// - `aliased` — a second name taken inside the loop. It said `let copy = out`
///   until RFC-0089 rule 1 made that a move, and now says `out.copy()`, which is
///   the named fix. The bytes it must print are the same either way, and that is
///   what this row pins: a per-turn snapshot of a growing accumulator reads the
///   accumulator as it stood, on every engine.
///
/// Byte equality across all three engines is the assertion, because a wrong
/// `(len, cap)` does not trap — it prints a truncated or doubled string, which is
/// the failure mode no amount of "it ran" would have caught.
#[test]
#[ignore = "needs clang; run explicitly: cargo test -p vyrn-cli --test parity -- --ignored"]
fn a_string_accumulator_grown_in_place_says_the_same_bytes_on_all_three_engines() {
    let rows = three_engines(
        "strappend",
        "accum",
        r#"
fn build(n: Int64) -> String {
    let mut out = "["
    let mut i = 0
    while i < n {
        out = out + i.toString() + ","
        i = i + 1
    }
    return out + "]"
}

fn reassigned() -> String {
    let mut out = "a"
    out = out + "b"
    out = "c"
    out = out + "d"
    return out + "!"
}

fn perTurn(n: Int64) -> String {
    let mut all = ""
    let mut i = 0
    while i < n {
        let mut row = "<"
        row = row + i.toString()
        all = all + row + ">"
        i = i + 1
    }
    return all
}

fn aliased(n: Int64) -> String {
    let mut out = "["
    let mut i = 0
    while i < n {
        out = out + "x"
        let copy = out.copy()
        print(copy)
        i = i + 1
    }
    return out
}

fn main() -> Int64 {
    print(build(0))
    print(build(1))
    print(build(4))
    print(reassigned())
    print(perTurn(3))
    print(aliased(3))
    return 0
}
"#,
    );
    assert_eq!(
        rows.len(),
        3,
        "wasmtime did not resolve, so wasm was never tested: {:?}",
        rows.iter().map(|r| r.0).collect::<Vec<_>>()
    );
    all_agree(&rows, "accum");
    assert_eq!(
        rows[0].1, "[]\n[0,]\n[0,1,2,3,]\ncd!\n<0><1><2>\n[x\n[xx\n[xxx\n[xxx\n",
        "an empty loop, one turn, four turns, a reset mid-way, a per-turn `let`, \
         and the aliased accumulator that still copies"
    );
}

/// `toJson` of a large array is LINEAR on all three engines, pinned by the wasm
/// address space rather than by a clock (RFC-0081).
///
/// The copying lowering re-`malloc`s and re-copies the whole result per element,
/// so the bytes it allocates go as N·L/2 — at 100k `Int64` that is about 34 GB,
/// and wasm32 has 4 GiB. The direct backend's allocator is a bump pointer that
/// never frees (see `direct::runtime`'s `malloc`), so those bytes are not
/// reclaimed and the run does not get slow, it walks off the end of linear memory:
/// 40k elements producing 229 KB of JSON trapped with `out of bounds memory
/// access` in 1.4 s. The in-place append allocates about 4L in total, which is
/// 2.7 MB here.
///
/// So this test cannot pass by being fast on a fast machine and cannot go flaky on
/// a loaded one — the ceiling it asserts against is a property of the target, not
/// of the host. That is why it is a size and not a duration. The alternative was
/// counting `call` instructions in the emitted code section, the way the textual
/// backend's pins count `strcat`; rejected because `wasm::Module::sweep` renumbers
/// every function index at `finish`, so the count would be over indices no source
/// of truth outside the finished bytes could name.
///
/// The full JSON is compared, not its length: a wrong `(len, cap)` shadow is a
/// truncation somewhere in the middle, and 684 KB of it is the shape most likely
/// to catch one.
#[test]
#[ignore = "needs clang; run explicitly: cargo test -p vyrn-cli --test parity -- --ignored"]
fn to_json_of_a_large_array_stays_within_the_wasm_address_space() {
    let rows = three_engines(
        "jsonbig",
        "big",
        r#"
fn main() -> Int64 {
    let mut a: Array<Int64> = []
    let mut i = 0
    while i < 100000 {
        a.push(i * 7 - 3)
        i = i + 1
    }
    let s = toJson(a)
    print(s.byteLength)
    print(s)
    return 0
}
"#,
    );
    assert_eq!(
        rows.len(),
        3,
        "wasmtime did not resolve, so wasm was never tested: {:?}",
        rows.iter().map(|r| r.0).collect::<Vec<_>>()
    );
    all_agree(&rows, "big");
    assert!(
        rows[0].1.starts_with("684125\n[-3,4,11,"),
        "the length and the first elements, so a passing run is one that produced \
         the JSON rather than one that produced nothing"
    );
}

/// A `malloc` that cannot grow linear memory TRAPS, in the native shim's words
/// (RFC-0081).
///
/// `memory.grow` returns the previous page count, or -1. The growth loop dropped
/// that result, so a refused grow left `memory.size` unchanged, the loop condition
/// still failed, and it asked again — forever, with nothing on either channel.
/// Uncapped that never showed: growth ran to the 4 GiB ceiling and the wrapped
/// bump pointer trapped `out of bounds memory access` instead, which is the trap
/// the test above was reading. A browser `WebAssembly.Memory` is routinely
/// constructed with a `maximum`, and the browser is a first-class target, so the
/// capped memory is the case that matters and the hang is what a user would see.
///
/// Capping it from the CLI is the awkward part: `-O memory-reservation` sets the
/// initial reservation and growth past it still succeeds, and
/// `-O pooling-max-memory-size` is ignored unless the pooling allocator is the one
/// allocating — hence both flags. The alternative was emitting a `maximum` on the
/// memory declaration, rejected because it would change what every module says in
/// order to test one of them.
///
/// `-W timeout` is what keeps a regression from *hanging* the suite: without it a
/// returned `drop` blocks `output()` and the run reads as stuck rather than as
/// broken. It cannot make this test pass, either — a timed-out run is wasmtime's
/// own `wasm trap: interrupt` on exit 3, and the assertions below are an exact
/// message on exit 1.
#[test]
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --test parity -- --ignored"]
fn a_malloc_that_cannot_grow_memory_traps_instead_of_growing_forever() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = scratch("directwasm-oom");
    // Doubling, so the cap is reached in ~25 allocations rather than by a loop
    // whose trip count would have to be tuned to the cap.
    let src = "\
fn main() -> Int64 {
    let mut s = \"x\"
    let mut i = 0
    while i < 40 {
        s = s + s
        i = i + 1
    }
    print(i)
    return 0
}
";
    let path = dir.join("oom.vyrn");
    std::fs::write(&path, src).unwrap();
    let module = dir.join("oom.wasm");
    let build = vyrn()
        .arg("build")
        .arg(&path)
        .arg("--target")
        .arg("wasm")
        .arg("-o")
        .arg(&module)
        .output()
        .expect("build wasm");
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let out = Command::new(&wasmtime)
        .arg("run")
        .arg("-W")
        .arg("timeout=20s")
        .arg("-O")
        .arg("pooling-allocator=y")
        .arg("-O")
        .arg("pooling-max-memory-size=16777216")
        .arg(&module)
        .output()
        .expect("run wasmtime");

    assert_eq!(
        norm(&out.stderr),
        "error: out of memory\n",
        "a refused grow must trap; `wasm trap: interrupt` here means the loop is \
         spinning again"
    );
    assert_eq!(out.status.code(), Some(1), "exit 1, like every trap");
    assert!(
        out.stdout.is_empty(),
        "the loop never completes, so nothing is printed"
    );
    // Parity compares stderr byte for byte, so the wasm wording is not a spelling
    // chosen here — it is the one `__vyrn_alloc_check` already prints for the same
    // failure on native.
    assert!(
        vyrn_codegen::toolchain::runtime_shim().contains("\"error: out of memory\\n\""),
        "the native shim no longer prints what this asserts"
    );
}

/// A `malloc` whose bump pointer would WRAP traps instead of handing back a
/// pointer to memory it never reserved (RFC-0081).
///
/// The native shim checks the size before the `(size_t)` cast and says why:
/// "a huge size could wrap to a tiny allocation - a buffer overflow, not an
/// error". The direct backend's `malloc` took an `i32`, so that cast had already
/// happened at the call site and there was nothing left for it to check —
/// `HEAP + align8(n)` simply wrapped, the `memory.size` test below it passed
/// because the wrapped top is small, and the caller got a valid-looking pointer
/// for an allocation it then wrote far past.
///
/// The size is now an `i64` — the signature `__vyrn_malloc` has had on native all
/// along — and the two requests here are the two ways it can fail:
///
///   - 5 GiB does not fit in a wasm32 address space at all. This is the width
///     check, and it must come BEFORE the rounding, because `n + 7` is this
///     backend's version of the cast: 2^64-1 rounds to 0, bumps the heap by
///     nothing, and returns a pointer for sixteen exabytes.
///   - 4294967280 fits in 32 bits but `HEAP + it` does not. Pre-fix this was the
///     nastier one, because NO allocation was attempted: the sum wrapped, the
///     heap moved BACKWARD by sixteen bytes, and `malloc` returned success
///     without touching `memory.grow` — which is why this pin cannot be
///     satisfied by an allocation merely succeeding, and why it needs no memory
///     cap the way the `memory.grow` pin above does.
///
/// The third call is the control: 64 bytes still allocates and still returns a
/// pointer, so a `malloc` that traps unconditionally does not pass either.
///
/// `--invoke` is the only way to reach `malloc` with a size no Vyrn program can
/// name — a program can only ask for what it could hold — and it is exactly the
/// path a JS caller takes: `wasi-min.js` calls the exported `__vyrn_malloc` with
/// a BigInt before passing a String in. That export used to go out through an
/// `i32.wrap` wrapper, so the browser boundary was where an oversized request
/// was silently narrowed.
#[test]
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --test parity -- --ignored"]
fn a_malloc_whose_bump_pointer_would_wrap_traps_instead_of_lying() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = scratch("directwasm-wrap");
    // A `String` parameter on an `export extern fn` is the condition under which
    // `__vyrn_malloc` is exported at all (asserted separately by
    // `the_wasm_module_exports_what_the_llvm_path_exports`).
    let src = "\
export extern fn greet(name: String) -> String {
    return name.copy()
}

fn main() -> Int64 {
    print(greet(\"hi\"))
    return 0
}
";
    let path = dir.join("wrap.vyrn");
    std::fs::write(&path, src).unwrap();
    let module = dir.join("wrap.wasm");
    let build = vyrn()
        .arg("build")
        .arg(&path)
        .arg("--target")
        .arg("wasm")
        .arg("-o")
        .arg(&module)
        .output()
        .expect("build wasm");
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let call = |n: &str| {
        Command::new(&wasmtime)
            .arg("run")
            .arg("-W")
            .arg("timeout=20s")
            .arg("--invoke")
            .arg("__vyrn_malloc")
            .arg(&module)
            .arg(n)
            .output()
            .expect("run wasmtime")
    };

    for n in ["5000000000", "4294967280"] {
        let out = call(n);
        // `ends_with`, not equality: wasmtime prints its own `--invoke is
        // experimental` warning on the same channel, and that is its wording to
        // change, not ours.
        assert!(
            norm(&out.stderr).ends_with("error: out of memory\n"),
            "malloc({n}) must trap, got:\n{}",
            norm(&out.stderr)
        );
        assert_eq!(
            out.status.code(),
            Some(1),
            "malloc({n}): exit 1, like every trap"
        );
        assert!(
            out.stdout.is_empty(),
            "malloc({n}) returned a pointer: {:?}",
            norm(&out.stdout)
        );
    }

    let ok = call("64");
    assert_eq!(
        ok.status.code(),
        Some(0),
        "64 bytes is an ordinary allocation"
    );
    assert!(
        !norm(&ok.stdout).trim().is_empty(),
        "64 bytes must still come back as a pointer, or the check is just a trap"
    );
    // Same single-sourcing as the pin above: the wording is the native shim's.
    assert!(
        vyrn_codegen::toolchain::runtime_shim().contains("\"error: out of memory\\n\""),
        "the native shim no longer prints what this asserts"
    );
}

/// Two instantiations whose readable mangles collide are still two bodies, on
/// every engine.
///
/// `mangle_ty` spells `Option<Int64>` as `OptInt64`, which is exactly what a user
/// type named `OptInt64` spells too — and the textual driver deduped its
/// monomorphization worklist on that string (`emitted.insert(sym)`), so the
/// second instantiation was never emitted and both call sites called the first.
/// `vyrn check` printed `ok`. The interpreter (which never mangles) and the
/// direct wasm backend (which keys its instantiation cache on the type arguments
/// themselves) both printed `9`; native read the one-word record `{ a: 9 }`
/// through the `Option<Int64>` body's `{ i1, i64, i64 }` layout and printed a
/// different number of stack garbage per run. LLVM does not object: a `call`
/// carries its own function type, so one `define` under two argument types
/// assembles without a diagnostic and the mismatch is undefined behaviour at run
/// time. A record with a `String` field would make the same read a wild pointer.
///
/// The `match` on the `Option` instantiation's element is the other direction of
/// the same confusion, and it is the arm that would fail loudly.
///
/// Not an example file, deliberately. The corpus reaches this shape nowhere — it
/// declares no type whose name imitates a mangle prefix, and nothing makes the
/// imitation a compile error — so a program that reaches it has to be written
/// down somewhere, and this is the tier that runs all three engines. The unit
/// claim underneath it (no two distinct types produce one symbol, over generated
/// type trees) is `vyrn-codegen`'s
/// `a_mangled_symbol_is_injective_over_generated_types`.
#[test]
#[ignore = "needs clang; run explicitly: cargo test -p vyrn-cli --test parity -- --ignored"]
fn two_instantiations_that_mangle_alike_are_still_two_bodies() {
    let rows = three_engines(
        "mangle",
        "collide",
        r#"
type OptInt64 = { a: Int64 }

fn dup<T>(x: T) -> Array<T> {
    let mut xs: Array<T> = []
    xs.push(x)
    return xs
}

fn main() -> Int64 {
    let o: Option<Int64> = Some(5)
    let r = OptInt64 { a: 9 }
    let xs = dup(o)
    let ys = dup(r)
    print("\{xs.length} \{ys.length}")
    print("\{ys[0].a}")
    let m = match xs[0] {
        Some(v) => v,
        None => -1,
    }
    print("\{m}")
    return 0
}
"#,
    );
    all_agree(&rows, "collide");
    assert_eq!(
        rows[0].1, "1 1\n9\n5\n",
        "the record's field, then the option's payload"
    );
}

/// RFC-0114 SS25's completeness instrument, pinned from both sides: under
/// `VYRN_LEAK_CHECK=1` a clean program (heap locals, heap module state — the
/// teardown's job) exits 0 with an empty audit table, and a program holding
/// the fold's recorded loop-store conservatism (`s = p.name.copy()` inside a
/// `for` never releases the displaced copy) exits 135 naming the leak. The
/// instrument being two-sided is what makes its silence on a program MEAN
/// something.
#[test]
#[ignore]
fn leak_check_is_two_sided() {
    let dir = std::env::temp_dir().join("vyrn-leakcheck");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let build_and_run = |name: &str, src: &str| {
        let path = dir.join(format!("{name}.vyrn"));
        std::fs::write(&path, src).expect("write");
        let exe = dir.join(format!("{name}.exe"));
        let build = vyrn()
            .arg("build")
            .arg(&path)
            .arg("-o")
            .arg(&exe)
            .output()
            .expect("build");
        assert!(build.status.success(), "{name}: {}", norm(&build.stderr));
        Command::new(&exe)
            .env("VYRN_LEAK_CHECK", "1")
            .output()
            .expect("run")
    };
    let clean = build_and_run(
        "leakclean",
        r#"let mut tally: Array<Int64> = [1, 2, 3]
let mut label = "module" + " state"

fn main() -> Int64 {
    let s = "a" + "b"
    tally.push(s.byteLength)
    label = label + "!"
    print(label.byteLength)
    return 0
}
"#,
    );
    assert_eq!(
        clean.status.code(),
        Some(0),
        "clean program must pass the empty-table assertion:
{}",
        norm(&clean.stderr)
    );
    let leaky = build_and_run(
        "leakleak",
        r#"type P = { name: String }

fn main() -> Int64 {
    let people = [P { name: "a name long enough to allocate" }]
    let mut s = ""
    let mut i = 0
    while i < 3 {
        for p in people {
            s = p.name.copy()
        }
        i = i + 1
    }
    print(s.byteLength)
    return 0
}
"#,
    );
    assert_eq!(
        leaky.status.code(),
        Some(135),
        "the recorded loop-store conservatism must be VISIBLE to the instrument:
{}",
        norm(&leaky.stderr)
    );
    assert!(
        norm(&leaky.stderr).contains("never freed"),
        "{}",
        norm(&leaky.stderr)
    );
}
