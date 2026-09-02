//! The fixture comparison (RFC-0125 §2.6, M5): every example, run as compiled
//! wasm in the embedded engine, against its recorded output.
//!
//! Parity compares three engines with each other. This compares ONE engine
//! with what was recorded once from the interpreter — `examples/expected/
//! <name>.stdout`, `.stderr` and `.exit` — so it needs no clang, no external
//! `wasmtime` and no second engine, and a divergence names the line. Together
//! with `wasmhash.rs` (the same bytes on every platform) it is what the parity
//! job becomes once the interpreter is gone.
//!
//! Two modes, read from `VYRN_FIXTURES`:
//!
//!   - unset: run each example with `vyrn run --engine wasm` and compare.
//!   - `write`: run each with `vyrn run` (the interpreter) and replace the
//!     recorded files. Do this when an example's OUTPUT is meant to change, and
//!     commit the result beside the change.
//!
//! Every example runs under the corpus's conventions (tests/common): cwd is
//! `examples/`, stdin is `<name>.stdin` or closed, argv is `<name>.args`, and
//! the clock and seed are fixed. The file is named by its bare name, as
//! `wasmhash.rs` names it, so a diagnostic that quotes the path is the same in
//! every checkout. A refusal (`EXPECTED_CHECK_FAILURE`) is compared like any
//! other program: its output is the diagnostic, and both engines share the
//! load that prints it — `polyrecursion.vyrn` among them, since `vyrn run`
//! refuses what `vyrn check` refuses under either engine. The host-only
//! program (`WASM_ONLY`) is skipped, since no terminal supplies its `extern`
//! namespace.

mod common;
use common::*;
use std::path::PathBuf;

fn expected_dir() -> PathBuf {
    examples_dir().join("expected")
}

#[test]
fn every_example_prints_what_was_recorded() {
    let dir = examples_dir();
    let write = match std::env::var("VYRN_FIXTURES").as_deref() {
        Ok("write") => true,
        Ok(other) => panic!("VYRN_FIXTURES must be unset or `write`, got `{other}`"),
        Err(_) => false,
    };
    let expected = expected_dir();
    if write {
        std::fs::create_dir_all(&expected).unwrap();
    }

    let mut names: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "vyrn"))
        .collect();
    names.sort();
    assert!(!names.is_empty(), "no examples found in {}", dir.display());

    let mut failures: Vec<String> = Vec::new();
    let (mut compared, mut skipped) = (0usize, 0usize);
    for path in &names {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let stem = path.file_stem().unwrap().to_string_lossy().to_string();
        if let Some((_, why)) = WASM_ONLY.iter().find(|(n, _)| *n == name) {
            eprintln!("SKIP  {name}  (host-only: {why})");
            skipped += 1;
            continue;
        }
        let mut cmd = vyrn();
        cmd.arg("run");
        if !write {
            cmd.arg("--engine").arg("wasm");
        }
        cmd.arg(&name);
        cmd.args(read_args(&path.with_extension("args")));
        let out = run_io(cmd, &dir, &path.with_extension("stdin"));
        let (stdout, stderr) = (norm(&out.stdout), norm(&out.stderr));
        let code = out
            .status
            .code()
            .map_or("none".to_string(), |c| c.to_string());

        let (f_out, f_err, f_exit) = (
            expected.join(format!("{stem}.stdout")),
            expected.join(format!("{stem}.stderr")),
            expected.join(format!("{stem}.exit")),
        );
        if write {
            std::fs::write(&f_out, &stdout).unwrap();
            std::fs::write(&f_err, &stderr).unwrap();
            std::fs::write(&f_exit, format!("{code}\n")).unwrap();
            eprintln!("wrote {name}  (exit {code})");
            compared += 1;
            continue;
        }
        let want = |p: &PathBuf| -> String {
            std::fs::read(p).map(|b| norm(&b)).unwrap_or_else(|e| {
                panic!("{}: {e} — record with VYRN_FIXTURES=write", p.display())
            })
        };
        let (w_out, w_err) = (want(&f_out), want(&f_err));
        let w_code = want(&f_exit).trim().to_string();
        if stdout != w_out || stderr != w_err || code != w_code {
            failures.push(format!(
                "{name}: DIVERGED from the recorded output\n  exit: recorded {w_code} vs wasm {code}\n{}{}",
                first_diff("stdout", "recorded", &w_out, "wasm", &stdout).unwrap_or_default(),
                first_diff("stderr", "recorded", &w_err, "wasm", &stderr).unwrap_or_default(),
            ));
            continue;
        }
        compared += 1;
        eprintln!("ok    {name}");
    }
    eprintln!(
        "\nfixtures: {compared} {}, {skipped} skipped (host-only), {} failed",
        if write { "recorded" } else { "compared" },
        failures.len()
    );
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}
