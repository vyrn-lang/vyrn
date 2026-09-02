//! The wasm2c route's parity column (RFC-0125 §2.5; PLAN-0125-runtime §6 step
//! 3): every corpus program built with `vyrn build --route wasm2c` prints the
//! same stdout and stderr bytes and exits with the same code as its wasm under
//! the `wasmtime` CLI, the engine `parity.rs`'s wasm column runs.
//!
//! The comparison is on the raw bytes, not `norm`'s: both sides run the same
//! module, and the host of `wasi_host.c` writes what the guest wrote. What this
//! gate holds fixed is the host, because the wasm is the same file on both
//! sides — the route writes `<out>.wasm` beside the binary, and that is what
//! wasmtime runs here.
//!
//! Ignored by default like `parity.rs`: it needs clang, wasmtime, and a wabt
//! release with simde under `tools/` (or `$VYRN_WASM2C` and `$VYRN_SIMDE`). CI
//! has no wabt, so a missing tool is a SKIP, and `VYRN_REQUIRE_TOOLS` turns the
//! skip into a failure the way it does for every other tool:
//!
//!     cargo test -p vyrn-cli --release --test route -- --ignored --nocapture

mod common;
use common::*;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The route's tools, or the reason the run is a SKIP.
fn route_tools() -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let wasm2c = match vyrn_codegen::toolchain::wasm2c_from(&root) {
        Ok(found) => found.map(|t| t.exe),
        Err(e) => panic!("{e}"),
    };
    require_tools("wasm2c", "VYRN_WASM2C", wasm2c)?;
    let simde = vyrn_codegen::toolchain::simde_from(&root).map(|(p, _)| p);
    require_tools("simde", "VYRN_SIMDE", simde)?;
    require_tools("clang", "CLANG", vyrn_codegen::toolchain::find_clang())?;
    wasmtime()
}

#[test]
#[ignore = "needs clang, wasmtime, wasm2c and simde; run explicitly: cargo test -p vyrn-cli --release --test route -- --ignored"]
fn every_example_agrees_between_the_wasm2c_route_and_the_wasm_engine() {
    let Some(wasmtime) = route_tools() else {
        eprintln!("SKIP: the wasm2c route's tools are not all present");
        return;
    };
    let dir = examples_dir();
    let out_dir = scratch("route-corpus");

    let mut names: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "vyrn"))
        .collect();
    names.sort();
    assert!(!names.is_empty(), "no examples found in {}", dir.display());

    let mut failures: Vec<String> = Vec::new();
    let (mut checked, mut skipped) = (0usize, 0usize);
    for path in &names {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        // The same three exclusions as the parity loop. `NATIVE_UNSUPPORTED` is
        // not one of them: the route runs the wasm, which has the lowering.
        let skip = KNOWN_DIVERGENT
            .iter()
            .map(|(n, why)| (*n, *why))
            .chain(EXPECTED_CHECK_FAILURE.iter().map(|(n, why, _)| (*n, *why)))
            .chain(WASM_ONLY.iter().map(|(n, why)| (*n, *why)))
            .find(|(n, _)| *n == name);
        if let Some((_, why)) = skip {
            eprintln!("SKIP  {name}  ({why})");
            skipped += 1;
            continue;
        }
        let stdin_fixture = path.with_extension("stdin");
        let prog_args = read_args(&path.with_extension("args"));

        let exe = out_dir.join(format!("{name}.exe"));
        let build = vyrn()
            .arg("build")
            .arg(path)
            .arg("--route")
            .arg("wasm2c")
            .arg("-o")
            .arg(&exe)
            .output()
            .expect("build");
        if !build.status.success() {
            failures.push(format!(
                "{name}: wasm2c route build failed:\n{}{}",
                norm(&build.stdout),
                norm(&build.stderr)
            ));
            continue;
        }
        let mut route_cmd = Command::new(&exe);
        route_cmd.args(&prog_args);
        let r = run_io(route_cmd, &dir, &stdin_fixture);

        let module = exe.with_extension("wasm");
        let mut wasm_cmd = Command::new(&wasmtime);
        wasm_cmd.arg("run").arg("--dir").arg(".");
        wasm_cmd
            .arg("--env")
            .arg(format!("VYRN_FIXED_TIME={FIXED_TIME}"));
        wasm_cmd
            .arg("--env")
            .arg(format!("VYRN_FIXED_SEED={FIXED_SEED}"));
        wasm_cmd.arg(&module);
        wasm_cmd.args(&prog_args);
        let w = run_io(wasm_cmd, &dir, &stdin_fixture);

        let (r_code, w_code) = (r.status.code(), w.status.code());
        if r.stdout != w.stdout || r.stderr != w.stderr || r_code != w_code {
            let (r_out, w_out) = (norm(&r.stdout), norm(&w.stdout));
            let (r_err, w_err) = (norm(&r.stderr), norm(&w.stderr));
            failures.push(format!(
                "{name}: ROUTE DIVERGED\n  exit: wasm2c {r_code:?} vs wasm {w_code:?}\n{}{}",
                first_diff("stdout", "wasm2c", &r_out, "wasm", &w_out).unwrap_or_default(),
                first_diff("stderr", "wasm2c", &r_err, "wasm", &w_err).unwrap_or_default(),
            ));
            continue;
        }
        checked += 1;
        eprintln!("ok    {name}");
    }
    eprintln!(
        "\nroute: {checked} checked, {skipped} skipped, {} failed",
        failures.len()
    );
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}
