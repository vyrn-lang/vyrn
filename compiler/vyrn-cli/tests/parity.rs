//! Corpus parity harness: every example must behave byte-identically under the
//! interpreter (`vyrn run`, the reference semantics) and the native binary
//! (`vyrn build` + execute). Compares stdout, stderr, and exit code.
//!
//! Ignored by default (needs `clang` and builds every example — ~a minute):
//!
//!     cargo test -p vyrn-cli --test parity -- --ignored --nocapture
//!
//! Line endings are normalized (CRLF → LF): the interpreter writes LF while
//! the native binary inherits the platform's text-mode CRLF — a documented,
//! benign difference.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Examples currently expected to diverge, with the reason. Shrink this list —
/// never grow it silently. (Empty since trap unification: every trap prints
/// the same `error: ...` bytes to stderr in both backends.)
const KNOWN_DIVERGENT: &[(&str, &str)] = &[];

/// Examples that are INTENTIONAL compile errors — they demonstrate a diagnostic
/// (e.g. compile-time validation of a provably-invalid constant) and never
/// build, so they can't participate in run-time parity. They are excluded from
/// the parity loop and instead asserted to fail `vyrn check` by
/// [`expected_check_failures_do_fail`]. This is distinct from KNOWN_DIVERGENT
/// (which is about interp/native divergence of programs that DO run).
const EXPECTED_CHECK_FAILURE: &[(&str, &str)] =
    &[("validate_compile.vyrn", "compile-time rejection of a provably-invalid constant")];

/// Examples whose behavior is HOST-PROVIDED (RFC-0012 `extern`): only a browser
/// page supplies the `vyrn` import namespace, so three-way output parity cannot
/// apply — wasmtime provides WASI, not `vyrn`. Excluded from the parity loop;
/// instead [`wasm_only_examples_trap_identically`] asserts the decided
/// non-wasm semantics: interp and native both produce the canonical
/// `error: extern `name` is not available on this target` trap, byte-identical
/// to each other. The real browser behavior is exercised by `web/externdemo.html`.
/// KNOWN_DIVERGENT stays empty — this list is about *hosts*, not divergence.
const WASM_ONLY: &[(&str, &str)] =
    &[("externdemo.vyrn", "calls `extern` fns; only the browser provides the `vyrn` namespace")];

fn examples_dir() -> PathBuf {
    // vyrn-cli/ -> compiler/ -> repo root -> examples/
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples").canonicalize().unwrap()
}

fn vyrn() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vyrn"))
}

fn norm(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace("\r\n", "\n")
}

/// The fixed clock and seed the harness injects (RFC-0043) so a time/random
/// example is a byte-identical three-way parity citizen: `now()` returns exactly
/// these epoch millis and `randomSeed()` this seed, in interp/native/wasm alike
/// (each backend's shim honors the same env). `1_700_000_000_000` ms is
/// 2023-11-14T22:13:20Z.
const FIXED_TIME: &str = "1700000000000";
const FIXED_SEED: &str = "424242";

/// Run `cmd` with the RFC-0014 I/O conventions: cwd = `examples/` (so relative
/// paths in examples resolve identically under every backend) and stdin piped
/// from `examples/<name>.stdin` when that fixture exists, else closed (EOF) —
/// never inherited, so a `readLine()` example can't hang the harness. The
/// RFC-0043 fixed clock/seed are set for every backend process (native + interp
/// read them directly; the wasm run additionally forwards them into the guest —
/// see the `--env` args on the wasmtime command).
fn run_io(mut cmd: Command, dir: &Path, stdin_fixture: &Path) -> std::process::Output {
    cmd.current_dir(dir);
    cmd.env("VYRN_FIXED_TIME", FIXED_TIME);
    cmd.env("VYRN_FIXED_SEED", FIXED_SEED);
    if stdin_fixture.exists() {
        cmd.stdin(std::fs::File::open(stdin_fixture).expect("open stdin fixture"));
    } else {
        cmd.stdin(Stdio::null());
    }
    cmd.output().expect("run backend")
}

/// The wasm toolchain, when present: the wasi-libc sysroot (for `vyrn build
/// --target wasm`) and a wasmtime executable to run the module. Discovered
/// from `$WASI_SYSROOT` / `$VYRN_WASMTIME`, falling back to the repo's
/// `tools/` directory. `None` disables the third parity column with a notice
/// (machines without the toolchain still verify interp == native).
fn wasm_toolchain() -> Option<(PathBuf, PathBuf)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let sysroot = std::env::var("WASI_SYSROOT")
        .map(PathBuf::from)
        .ok()
        .filter(|p| p.exists())
        .or_else(|| Some(root.join("tools/wasi-sysroot-25.0")).filter(|p| p.exists()))?;
    let wasmtime = std::env::var("VYRN_WASMTIME")
        .map(PathBuf::from)
        .ok()
        .filter(|p| p.exists())
        .or_else(|| {
            Some(root.join("tools/wasmtime-v46.0.1-x86_64-windows/wasmtime.exe"))
                .filter(|p| p.exists())
        })?;
    Some((sysroot, wasmtime))
}

#[test]
#[ignore = "needs clang; run explicitly: cargo test -p vyrn-cli --test parity -- --ignored"]
fn examples_interp_native_parity() {
    let dir = examples_dir();
    let out_dir = std::env::temp_dir().join("vyrn-parity");
    std::fs::create_dir_all(&out_dir).unwrap();
    let wasm = wasm_toolchain();
    if wasm.is_none() {
        eprintln!("NOTE: wasm toolchain not found — verifying interp == native only");
    }

    let mut names: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "vyrn"))
        .collect();
    names.sort();
    assert!(!names.is_empty(), "no examples found in {}", dir.display());

    let mut failures: Vec<String> = Vec::new();
    let mut skipped = 0usize;

    for path in &names {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if let Some((_, why)) = KNOWN_DIVERGENT.iter().find(|(n, _)| *n == name) {
            eprintln!("SKIP  {name}  ({why})");
            skipped += 1;
            continue;
        }
        if let Some((_, why)) = EXPECTED_CHECK_FAILURE.iter().find(|(n, _)| *n == name) {
            eprintln!("SKIP  {name}  (expected check failure: {why})");
            skipped += 1;
            continue;
        }
        if let Some((_, why)) = WASM_ONLY.iter().find(|(n, _)| *n == name) {
            eprintln!("SKIP  {name}  (wasm-only: {why})");
            skipped += 1;
            continue;
        }

        // RFC-0014 conventions: `examples/<name>.stdin` pipes into all three
        // backends; every run's cwd is `examples/` so relative file paths in
        // the example resolve identically everywhere.
        let stdin_fixture = path.with_extension("stdin");

        let mut interp_cmd = vyrn();
        interp_cmd.arg("run").arg(path);
        let interp = run_io(interp_cmd, &dir, &stdin_fixture);

        let exe = out_dir.join(format!("{name}.exe"));
        let build = vyrn().arg("build").arg(path).arg("-o").arg(&exe).output().expect("build");
        if !build.status.success() {
            failures.push(format!(
                "{name}: native build failed:\n{}{}",
                norm(&build.stdout),
                norm(&build.stderr)
            ));
            continue;
        }
        let native = run_io(Command::new(&exe), &dir, &stdin_fixture);

        let (i_out, n_out) = (norm(&interp.stdout), norm(&native.stdout));
        let (i_err, n_err) = (norm(&interp.stderr), norm(&native.stderr));
        let (i_code, n_code) = (interp.status.code(), native.status.code());

        if i_out != n_out || i_err != n_err || i_code != n_code {
            failures.push(format!(
                "{name}: DIVERGED\n  exit: interp {i_code:?} vs native {n_code:?}\n  \
                 stdout interp: {i_out:?}\n  stdout native: {n_out:?}\n  \
                 stderr interp: {i_err:?}\n  stderr native: {n_err:?}"
            ));
            continue;
        }

        // Third column: the same program compiled to wasm32-wasi must match
        // the interpreter byte-for-byte too (wasm writes LF like the interp;
        // norm() makes it moot either way).
        if let Some((sysroot, wasmtime)) = &wasm {
            let module = out_dir.join(format!("{name}.wasm"));
            let build = vyrn()
                .arg("build")
                .arg(path)
                .arg("--target")
                .arg("wasm")
                .arg("-o")
                .arg(&module)
                .env("WASI_SYSROOT", sysroot)
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
            wasm_cmd.arg("--env").arg(format!("VYRN_FIXED_TIME={FIXED_TIME}"));
            wasm_cmd.arg("--env").arg(format!("VYRN_FIXED_SEED={FIXED_SEED}"));
            wasm_cmd.arg(&module);
            let w = run_io(wasm_cmd, &dir, &stdin_fixture);
            let (w_out, w_err) = (norm(&w.stdout), norm(&w.stderr));
            let w_code = w.status.code();
            if i_out != w_out || i_err != w_err || i_code != w_code {
                failures.push(format!(
                    "{name}: WASM DIVERGED\n  exit: interp {i_code:?} vs wasm {w_code:?}\n  \
                     stdout interp: {i_out:?}\n  stdout wasm: {w_out:?}\n  \
                     stderr interp: {i_err:?}\n  stderr wasm: {w_err:?}"
                ));
                continue;
            }
        }
        eprintln!("ok    {name}");
    }

    eprintln!(
        "\nparity: {} checked, {} skipped (known divergent), {} failed",
        names.len() - skipped,
        skipped,
        failures.len()
    );
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
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
    let out_dir = std::env::temp_dir().join("vyrn-parity");
    std::fs::create_dir_all(&out_dir).unwrap();
    for (name, _why) in WASM_ONLY {
        let path = dir.join(name);

        let interp = vyrn().arg("run").arg(&path).output().expect("run interp");
        assert_eq!(interp.status.code(), Some(1), "{name}: interp must trap (exit 1)");
        let i_err = norm(&interp.stderr);
        assert!(
            i_err.contains("is not available on this target"),
            "{name}: interp must print the canonical extern trap, got:\n{i_err}"
        );

        let exe = out_dir.join(format!("{name}.exe"));
        let build = vyrn().arg("build").arg(&path).arg("-o").arg(&exe).output().expect("build");
        assert!(
            build.status.success(),
            "{name}: native build must succeed (extern trap stubs link):\n{}",
            norm(&build.stderr)
        );
        let native = Command::new(&exe).output().expect("run native");
        assert_eq!(native.status.code(), Some(1), "{name}: native must trap (exit 1)");
        assert_eq!(
            norm(&native.stderr),
            i_err,
            "{name}: interp and native extern traps must be byte-identical"
        );
        assert_eq!(norm(&native.stdout), norm(&interp.stdout), "{name}: stdout identical too");
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
    let out_dir = std::env::temp_dir().join("vyrn-parity");
    std::fs::create_dir_all(&out_dir).unwrap();
    let path = dir.join("parallel.vyrn");

    let interp = vyrn().arg("run").arg(&path).output().expect("run interp");
    let exe = out_dir.join("parallel-seq-check.exe");
    let build = vyrn().arg("build").arg(&path).arg("-o").arg(&exe).output().expect("build");
    assert!(build.status.success(), "native build failed:\n{}", norm(&build.stderr));

    let threaded = Command::new(&exe).output().expect("run threaded");
    let sequential = Command::new(&exe)
        .env("VYRN_SEQUENTIAL_SPAWN", "1")
        .output()
        .expect("run sequential");

    for (label, run) in [("threaded", &threaded), ("VYRN_SEQUENTIAL_SPAWN=1", &sequential)] {
        assert_eq!(norm(&run.stdout), norm(&interp.stdout), "{label}: stdout != interp");
        assert_eq!(norm(&run.stderr), norm(&interp.stderr), "{label}: stderr != interp");
        assert_eq!(run.status.code(), interp.status.code(), "{label}: exit code != interp");
    }
}

#[test]
#[ignore = "needs clang; run explicitly: cargo test -p vyrn-cli --test parity -- --ignored"]
fn task_trap_prints_once_and_exits_1_threaded() {
    let out_dir = std::env::temp_dir().join("vyrn-parity");
    std::fs::create_dir_all(&out_dir).unwrap();
    // A long task in flight while a second task traps: the trapping task
    // performs the standard trap protocol itself (stderr + exit(1)) from its
    // own thread — the locked RFC-0025 semantics.
    let src = "fn boom(n: Int64) -> Int64 {\n    let z = n - n\n    return n / z\n}\n\n\
               fn fib(n: Int64) -> Int64 {\n    if n < 2 { return n }\n    \
               return fib(n - 1) + fib(n - 2)\n}\n\n\
               fn main() -> Int64 {\n    let w = spawn fib(30)\n    \
               let t = spawn boom(3)\n    return t.join()\n}\n";
    let file = out_dir.join("taskboom.vyrn");
    std::fs::write(&file, src).unwrap();
    let exe = out_dir.join("taskboom.exe");
    let build = vyrn().arg("build").arg(&file).arg("-o").arg(&exe).output().expect("build");
    assert!(build.status.success(), "native build failed:\n{}", norm(&build.stderr));

    let interp = vyrn().arg("run").arg(&file).output().expect("run interp");
    let threaded = Command::new(&exe).output().expect("run threaded");
    let sequential = Command::new(&exe)
        .env("VYRN_SEQUENTIAL_SPAWN", "1")
        .output()
        .expect("run sequential");

    for (label, run) in
        [("interp", &interp), ("threaded", &threaded), ("VYRN_SEQUENTIAL_SPAWN=1", &sequential)]
    {
        assert_eq!(run.status.code(), Some(1), "{label}: task trap must exit 1");
        assert_eq!(norm(&run.stdout), "", "{label}: no stdout");
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
    let out_dir = std::env::temp_dir().join("vyrn-parity");
    std::fs::create_dir_all(&out_dir).unwrap();

    // (payload type, sample value, callback body reading the payload). Each stores
    // `cb` (a fn-param) into module-state Map<String, fn(Payload)>, then retrieves
    // and calls it — the exact shape the std/rpc v2 client emits.
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
             fn on(k: String, cb: Sink) {{\n    pending[k] = cb\n}}\n\
             fn fire(k: String, p: {payload_ty}) {{\n    \
             match pending[k] {{ Some(cb) => cb(p), None => print(\"none\") }}\n}}\n\
             fn main() -> Int64 {{\n    on(\"a\", |p| {body})\n    \
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
        let build =
            vyrn().arg("build").arg(&file).arg("-o").arg(&exe).output().expect("build");
        assert!(
            build.status.success(),
            "{label}: native build of a stored fn-param ({payload_ty}) must succeed \
             (the RFC-0040 §2 wall), got:\n{}{}",
            norm(&build.stdout),
            norm(&build.stderr)
        );
        let native = Command::new(&exe).output().expect("run native");
        assert_eq!(
            norm(&native.stdout),
            norm(&interp.stdout),
            "{label}: native stdout must match the interpreter"
        );
        assert_eq!(native.status.code(), interp.status.code(), "{label}: exit code");
    }
}

#[test]
fn expected_check_failures_do_fail() {
    let dir = examples_dir();
    for (name, _why) in EXPECTED_CHECK_FAILURE {
        let path = dir.join(name);
        let out = vyrn().arg("check").arg(&path).output().expect("run check");
        assert!(
            !out.status.success(),
            "{name}: expected `vyrn check` to fail, but it passed"
        );
        let err = norm(&out.stderr) + &norm(&out.stdout);
        assert!(
            err.contains("does not satisfy"),
            "{name}: expected a validation diagnostic, got:\n{err}"
        );
    }
}
