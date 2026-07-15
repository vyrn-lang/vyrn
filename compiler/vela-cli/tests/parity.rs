//! Corpus parity harness: every example must behave byte-identically under the
//! interpreter (`velac run`, the reference semantics) and the native binary
//! (`velac build` + execute). Compares stdout, stderr, and exit code.
//!
//! Ignored by default (needs `clang` and builds every example — ~a minute):
//!
//!     cargo test -p vela-cli --test parity -- --ignored --nocapture
//!
//! Line endings are normalized (CRLF → LF): the interpreter writes LF while
//! the native binary inherits the platform's text-mode CRLF — a documented,
//! benign difference.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Examples currently expected to diverge, with the reason. Shrink this list —
/// never grow it silently. (Empty since trap unification: every trap prints
/// the same `error: ...` bytes to stderr in both backends.)
const KNOWN_DIVERGENT: &[(&str, &str)] = &[];

/// Examples that are INTENTIONAL compile errors — they demonstrate a diagnostic
/// (e.g. compile-time validation of a provably-invalid constant) and never
/// build, so they can't participate in run-time parity. They are excluded from
/// the parity loop and instead asserted to fail `velac check` by
/// [`expected_check_failures_do_fail`]. This is distinct from KNOWN_DIVERGENT
/// (which is about interp/native divergence of programs that DO run).
const EXPECTED_CHECK_FAILURE: &[(&str, &str)] =
    &[("validate_compile.vela", "compile-time rejection of a provably-invalid constant")];

fn examples_dir() -> PathBuf {
    // vela-cli/ -> compiler/ -> repo root -> examples/
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples").canonicalize().unwrap()
}

fn velac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_velac"))
}

fn norm(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace("\r\n", "\n")
}

/// The wasm toolchain, when present: the wasi-libc sysroot (for `velac build
/// --target wasm`) and a wasmtime executable to run the module. Discovered
/// from `$WASI_SYSROOT` / `$VELA_WASMTIME`, falling back to the repo's
/// `tools/` directory. `None` disables the third parity column with a notice
/// (machines without the toolchain still verify interp == native).
fn wasm_toolchain() -> Option<(PathBuf, PathBuf)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let sysroot = std::env::var("WASI_SYSROOT")
        .map(PathBuf::from)
        .ok()
        .filter(|p| p.exists())
        .or_else(|| Some(root.join("tools/wasi-sysroot-25.0")).filter(|p| p.exists()))?;
    let wasmtime = std::env::var("VELA_WASMTIME")
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
#[ignore = "needs clang; run explicitly: cargo test -p vela-cli --test parity -- --ignored"]
fn examples_interp_native_parity() {
    let dir = examples_dir();
    let out_dir = std::env::temp_dir().join("vela-parity");
    std::fs::create_dir_all(&out_dir).unwrap();
    let wasm = wasm_toolchain();
    if wasm.is_none() {
        eprintln!("NOTE: wasm toolchain not found — verifying interp == native only");
    }

    let mut names: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "vela"))
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

        let interp = velac().arg("run").arg(path).output().expect("run interp");

        let exe = out_dir.join(format!("{name}.exe"));
        let build = velac().arg("build").arg(path).arg("-o").arg(&exe).output().expect("build");
        if !build.status.success() {
            failures.push(format!(
                "{name}: native build failed:\n{}{}",
                norm(&build.stdout),
                norm(&build.stderr)
            ));
            continue;
        }
        let native = Command::new(&exe).output().expect("run native");

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
            let build = velac()
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
            let w = Command::new(wasmtime).arg(&module).output().expect("run wasm");
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

/// The intentional-compile-error examples must actually fail `velac check` (and
/// name a validation diagnostic) — a guard so a silently-fixed example doesn't
/// keep claiming to demonstrate a rejection. Runs without clang, so it is not
/// `#[ignore]`d.
#[test]
fn expected_check_failures_do_fail() {
    let dir = examples_dir();
    for (name, _why) in EXPECTED_CHECK_FAILURE {
        let path = dir.join(name);
        let out = velac().arg("check").arg(&path).output().expect("run check");
        assert!(
            !out.status.success(),
            "{name}: expected `velac check` to fail, but it passed"
        );
        let err = norm(&out.stderr) + &norm(&out.stdout);
        assert!(
            err.contains("does not satisfy"),
            "{name}: expected a validation diagnostic, got:\n{err}"
        );
    }
}
