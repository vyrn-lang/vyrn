//! Every example, run as compiled wasm in the embedded engine, against its
//! recorded `examples/expected/<name>.stdout`, `.stderr` and `.exit`.
//!
//! It needs no clang and no external `wasmtime`, and a divergence names the line.
//! The route records its own expectation, so this proves the answer has not moved
//! since a human reviewed it, not that it is right.
//!
//! `VYRN_FIXTURES` unset compares; `VYRN_FIXTURES=write` replaces the recorded
//! files. Write only when an example's output is meant to change, and commit the
//! files beside the change so the diff is reviewed.
//!
//! Examples run under the corpus conventions (tests/common) and are named by their
//! bare name, so a diagnostic that quotes the path is the same in every checkout.
//! Refusals and `WASM_ONLY` programs are compared like the rest: the embedded host
//! answers an `extern` with the recorded refusal.

mod common;
use common::*;
use std::path::PathBuf;

fn expected_dir() -> PathBuf {
    examples_dir().join("expected")
}

#[test]
#[ignore = "compiles and runs the whole corpus; the `fixtures` job runs it: cargo test -p vyrn-cli --test fixtures -- --ignored"]
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
    let mut compared = 0usize;
    for path in &names {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let stem = path.file_stem().unwrap().to_string_lossy().to_string();
        let mut cmd = vyrn();
        cmd.arg("run").arg(&name);
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
            // A trap's message can sit past the first line that differs (#444).
            let whole = if stderr != w_err {
                format!("  the run's whole stderr:\n{stderr}")
            } else {
                String::new()
            };
            failures.push(format!(
                "{name}: DIVERGED from the recorded output\n  exit: recorded {w_code} vs run {code}\n{}{}{whole}",
                first_diff("stdout", "recorded", &w_out, "run", &stdout).unwrap_or_default(),
                first_diff("stderr", "recorded", &w_err, "run", &stderr).unwrap_or_default(),
            ));
            continue;
        }
        compared += 1;
        eprintln!("ok    {name}");
    }
    eprintln!(
        "\nfixtures: {compared} {}, {} failed",
        if write { "recorded" } else { "compared" },
        failures.len()
    );
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

/// A module has one instance per process, and `vyrn test` is one process, so a
/// body reads what an earlier body wrote.
#[test]
fn module_state_is_shared_across_test_bodies() {
    let dir = scratch("module-state");
    let probe = dir.join("state.vyrn");
    std::fs::write(
        &probe,
        r#"let mut counter = 0

test "the first body writes the module's state" {
    counter = counter + 1
    assertEq(counter, 1)
}

test "the second body sees what the first wrote" {
    counter = counter + 1
    assertEq(counter, 2)
}
"#,
    )
    .expect("write the program");
    let out = vyrn()
        .arg("test")
        .arg(&probe)
        .output()
        .expect("run vyrn test");
    let text = norm(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "the run:\n{text}");
    assert!(text.contains("2 passed, 0 failed"), "the run:\n{text}");
}
