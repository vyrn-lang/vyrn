//! The fixture comparison (RFC-0125 §2.6, M5): every example, run as compiled
//! wasm in the embedded engine, against its recorded output.
//!
//! Parity compares three engines with each other. This compares ONE engine
//! with a recorded file — `examples/expected/<name>.stdout`, `.stderr` and
//! `.exit` — so it needs no clang, no external `wasmtime` and no second engine,
//! and a divergence names the line. Together with `wasmhash.rs` (the same bytes
//! on every platform) it is what the parity job becomes once the interpreter is
//! gone.
//!
//! The recorded file is the expectation, and the ROUTE records it (RFC-0125 §3
//! M5, the ninth slice). What that proves is that the route's answer has not
//! moved since a human read it in a diff; what it does not prove is that the
//! answer is right, which no self-comparison can. The interpreter is an
//! optional second column for as long as there is an interpreter, and it is a
//! second opinion rather than the oracle.
//!
//! Three modes, read from `VYRN_FIXTURES`:
//!
//!   - unset: run each example with `vyrn run --engine wasm` and compare.
//!   - `write`: run each the same way and replace the recorded files. Do this
//!     when an example's OUTPUT is meant to change, and commit the result
//!     beside the change — where it is reviewed, which is what makes it an
//!     expectation.
//!   - `interp`: compare `vyrn run` with the same recorded files. The second
//!     column.
//!
//! Every example runs under the corpus's conventions (tests/common): cwd is
//! `examples/`, stdin is `<name>.stdin` or closed, argv is `<name>.args`, and
//! the clock and seed are fixed. The file is named by its bare name, as
//! `wasmhash.rs` names it, so a diagnostic that quotes the path is the same in
//! every checkout. A refusal (`EXPECTED_CHECK_FAILURE`) is compared like any
//! other program: its output is the diagnostic, and both engines share the
//! load that prints it — `polyrecursion.vyrn` among them, since `vyrn run`
//! refuses what `vyrn check` refuses under either engine. Nothing is skipped:
//! the host-only program (`WASM_ONLY`) is compared like the rest, because the
//! embedded host answers an RFC-0012 `extern` with the same refusal the
//! interpreter recorded (RFC-0125 §3 M5, the `extern-unavailable` row).

mod common;
use common::*;
use std::path::PathBuf;

fn expected_dir() -> PathBuf {
    examples_dir().join("expected")
}

/// One row of RFC-0125 §3 M5's census: a capability the interpreter provides,
/// and what the compiled route does with it. `verdict` is the first word of the
/// row's third cell — `yes`, `no`, `partial` or `worse`.
struct Census {
    capability: &'static str,
    verdict: &'static str,
}

/// Every row of the census, in the order the RFC's table lists them.
///
/// The verdicts were proved by running both engines over the same input, not by
/// reading the code — the slice's report says which run proved which row.
const CENSUS: &[Census] = &[
    Census {
        capability: "run-default",
        verdict: "yes",
    },
    Census {
        capability: "test-bodies",
        verdict: "yes",
    },
    Census {
        capability: "test-state",
        verdict: "yes",
    },
    Census {
        capability: "bench-check",
        verdict: "yes",
    },
    Census {
        capability: "serve",
        verdict: "yes",
    },
    Census {
        capability: "mounted-routes",
        verdict: "yes",
    },
    Census {
        capability: "from-json",
        verdict: "yes",
    },
    Census {
        capability: "run-profile",
        verdict: "yes",
    },
    Census {
        capability: "gen-fn",
        verdict: "partial",
    },
    Census {
        capability: "fixture-oracle",
        verdict: "yes",
    },
    Census {
        capability: "parity-column",
        verdict: "yes",
    },
    Census {
        capability: "boundary-carrier",
        verdict: "yes",
    },
    Census {
        capability: "library-run",
        verdict: "yes",
    },
    Census {
        capability: "extern-unavailable",
        verdict: "yes",
    },
    Census {
        capability: "site-export",
        verdict: "yes",
    },
];

#[test]
#[ignore = "compiles and runs the whole corpus; the `fixtures` job runs it: cargo test -p vyrn-cli --test fixtures -- --ignored"]
fn every_example_prints_what_was_recorded() {
    let dir = examples_dir();
    let (write, engine) = match std::env::var("VYRN_FIXTURES").as_deref() {
        Ok("write") => (true, "wasm"),
        Ok("interp") => (false, "interp"),
        Ok(other) => panic!("VYRN_FIXTURES must be unset, `write` or `interp`, got `{other}`"),
        Err(_) => (false, "wasm"),
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
        cmd.arg("run").arg("--engine").arg(engine).arg(&name);
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
                "{name}: DIVERGED from the recorded output\n  exit: recorded {w_code} vs {engine} {code}\n{}{}",
                first_diff("stdout", "recorded", &w_out, engine, &stdout).unwrap_or_default(),
                first_diff("stderr", "recorded", &w_err, engine, &stderr).unwrap_or_default(),
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

/// One census, not two: the table in RFC-0125 §3 M5 and [`CENSUS`] name the
/// same capabilities with the same verdicts.
///
/// `tests/boundaries.rs` holds its own census this way, and for the same
/// reason: a table in prose beside a table in code is two tables, and the one
/// nobody runs is the one that goes stale.
#[test]
fn the_rfc_census_lists_exactly_these_capabilities() {
    let rfc = repo_root()
        .join("rfcs")
        .join("RFC-0125-a-rule-is-stated-once.md");
    let text = std::fs::read_to_string(&rfc).unwrap_or_else(|e| panic!("{}: {e}", rfc.display()));
    let header = "| capability | who needs it | the compiled route today | what moving it costs |";
    let start = text
        .find(header)
        .unwrap_or_else(|| panic!("{}: no census table (looked for {header:?})", rfc.display()));
    let mut got: Vec<String> = Vec::new();
    for line in text[start..].lines().skip(2) {
        if !line.starts_with("| `") {
            break;
        }
        let cells: Vec<&str> = line
            .trim_matches('|')
            .split('|')
            .map(|c| c.trim())
            .collect();
        assert_eq!(cells.len(), 4, "row has {} cells: {line}", cells.len());
        // The verdict is the third cell's first word, so the rest of the cell
        // can say why without the pin caring.
        let verdict = cells[2]
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_end_matches([',', '.']);
        got.push(format!("{} {verdict}", cells[0].trim_matches('`')));
    }
    let want: Vec<String> = CENSUS
        .iter()
        .map(|r| format!("{} {}", r.capability, r.verdict))
        .collect();
    assert_eq!(
        got,
        want,
        "the census table in {} and `CENSUS` differ. One table, not two.",
        rfc.display()
    );
}

/// The census's one live semantic claim, run rather than asserted: the
/// `test-state` row.
///
/// RFC-0029 locks one module instance per PROCESS, and `vyrn test` is one
/// process, so a body reads what an earlier body wrote. Both engines answer
/// that now — the compiled route on one resident instance with a door per body
/// (RFC-0125 §3 M5, the ninth slice), where it used to run one fresh instance
/// per body and disagree. The probe under `rfcs/probes-0125/` is twelve lines
/// and is the whole claim.
#[test]
fn module_state_is_shared_across_test_bodies_on_both_engines() {
    let probe = repo_root()
        .join("rfcs")
        .join("probes-0125")
        .join("module-state-across-test-bodies.vyrn");
    let one = |engine: Option<&str>| {
        let mut cmd = vyrn();
        cmd.arg("test");
        if let Some(e) = engine {
            cmd.arg("--engine").arg(e);
        }
        let out = cmd.arg(&probe).output().expect("run vyrn test");
        (out.status.code(), norm(&out.stdout))
    };
    let (interp_code, interp_out) = one(None);
    assert_eq!(interp_code, Some(0), "the interpreter's run:\n{interp_out}");
    let (wasm_code, wasm_out) = one(Some("wasm"));
    assert_eq!(wasm_code, Some(0), "the compiled run:\n{wasm_out}");
    assert_eq!(interp_out, wasm_out, "the two engines disagree");
    assert!(
        interp_out.contains("2 passed, 0 failed"),
        "both runs:\n{interp_out}"
    );
}

/// The repository root, for the RFC and the probe the two tests above read.
fn repo_root() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}
