//! Every value-boundary check the engines carry: one row per rule, the engines
//! that state it themselves, and a program that fires it.
//!
//! A carrier is one engine's own statement of the condition; the wording is one
//! table (`vyrn_frontend::trap`). An engine that calls another carrier's
//! statement is not a carrier. The test runs each program under wasm and native
//! and asserts identical output. The native column needs clang and skips
//! without it, unless `VYRN_REQUIRE_TOOLS` is set.

mod common;
use common::*;

use std::path::{Path, PathBuf};

/// One engine's own statement of a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Carrier {
    Native,
    Wasm,
    Vyrn,
}

/// One row of the census: a rule, who states it, and a program that fires it.
pub struct Row {
    /// The rule's key, and the stem of its program under `tests/boundaries/`.
    pub rule: &'static str,
    /// Every engine that states the rule itself; `len()` is the copy count.
    pub carriers: &'static [Carrier],
}

/// Every value-boundary rule. The carriers were read,
/// not grepped: a mention of a wording is not a statement of a rule.
pub const ROWS: &[Row] = &[
    Row {
        rule: "array-index",
        carriers: &[Carrier::Native, Carrier::Wasm],
    },
    Row {
        rule: "string-index",
        carriers: &[Carrier::Native, Carrier::Wasm],
    },
    Row {
        rule: "int-div-zero",
        carriers: &[Carrier::Native, Carrier::Wasm],
    },
    Row {
        rule: "int-rem-zero",
        carriers: &[Carrier::Native, Carrier::Wasm],
    },
    Row {
        rule: "int-div-overflow",
        carriers: &[Carrier::Native, Carrier::Wasm],
    },
    Row {
        rule: "shift-range",
        carriers: &[Carrier::Native, Carrier::Wasm],
    },
    Row {
        rule: "int-narrowing",
        carriers: &[Carrier::Native, Carrier::Wasm],
    },
    Row {
        rule: "float-to-int",
        carriers: &[Carrier::Native, Carrier::Wasm],
    },
    // The predicate and its refusal are a generated Vyrn function per
    // declaration (`vyrn_frontend::ctor`).
    Row {
        rule: "where-scalar",
        carriers: &[Carrier::Vyrn],
    },
    Row {
        rule: "where-record",
        carriers: &[Carrier::Vyrn],
    },
    // The end of a group of stores into a record's fields calls the same
    // constructor.
    Row {
        rule: "where-group",
        carriers: &[Carrier::Vyrn],
    },
    // The check over the bytes is `std/text`'s `stringFault`, which every engine
    // calls; each engine keeps only the build, which allocates.
    Row {
        rule: "string-nul",
        carriers: &[Carrier::Vyrn],
    },
    Row {
        rule: "string-utf8",
        carriers: &[Carrier::Vyrn],
    },
    // The file rows keep their carriers: their bytes are never an `Array<UInt8>`,
    // so calling `stringFault` would copy every file read.
    Row {
        rule: "file-nul",
        carriers: &[Carrier::Native, Carrier::Vyrn],
    },
    Row {
        rule: "file-utf8",
        carriers: &[Carrier::Native, Carrier::Vyrn],
    },
    Row {
        rule: "io-status",
        carriers: &[Carrier::Native, Carrier::Vyrn],
    },
    Row {
        rule: "stack-exhausted",
        carriers: &[Carrier::Native, Carrier::Wasm],
    },
    Row {
        rule: "region-depth",
        carriers: &[Carrier::Native, Carrier::Wasm],
    },
    Row {
        rule: "json-decode",
        carriers: &[Carrier::Vyrn],
    },
    Row {
        rule: "char-boundary",
        carriers: &[Carrier::Vyrn],
    },
];

fn programs_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("boundaries")
        .canonicalize()
        .expect("tests/boundaries")
}

/// One run's comparable output, CRLF-normalised by [`norm`]: a native binary on
/// Windows writes `\r\n`.
#[derive(PartialEq, Eq)]
struct Answer {
    out: String,
    err: String,
    code: String,
}

impl std::fmt::Display for Answer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "exit {}\n  stdout {:?}\n  stderr {:?}",
            self.code, self.out, self.err
        )
    }
}

fn answer(out: std::process::Output) -> Answer {
    Answer {
        out: norm(&out.stdout),
        err: runtime_err(&out.stderr),
        code: out
            .status
            .code()
            .map_or("none".to_string(), |c| c.to_string()),
    }
}

#[test]
fn every_boundary_row_says_the_same_thing_in_every_engine() {
    let dir = programs_dir();
    let native = vyrn_codegen::toolchain::find_clang();
    let native = require_tools("clang", "VYRN_CLANG", native).is_some();
    if !native {
        eprintln!("SKIP the native column (no clang); the wasm column still runs");
    }
    let scratch = scratch("boundaries");

    let mut failures: Vec<String> = Vec::new();
    for row in ROWS {
        let file = format!("{}.vyrn", row.rule);
        assert!(
            dir.join(&file).exists(),
            "row `{}` has no program: {}",
            row.rule,
            dir.join(&file).display()
        );
        let wasm = {
            let mut cmd = vyrn();
            cmd.arg("run");
            cmd.arg(&file);
            answer(run_io(cmd, &dir, &dir.join("nostdin")))
        };
        if !native {
            eprintln!("ok    {:<16} wasm answered {wasm}", row.rule);
            continue;
        }
        let exe = scratch.join(format!("{}.exe", row.rule));
        let build = vyrn()
            .current_dir(&dir)
            .arg("build")
            .arg(&file)
            .arg("-o")
            .arg(&exe)
            .output()
            .expect("build");
        if !build.status.success() {
            failures.push(format!(
                "{}: native build failed:\n{}{}",
                row.rule,
                norm(&build.stdout),
                norm(&build.stderr)
            ));
            continue;
        }
        let got = answer(run_io(
            std::process::Command::new(&exe),
            &dir,
            &dir.join("nostdin"),
        ));
        if wasm != got {
            failures.push(format!(
                "{}: wasm and native differ\n  wasm   {wasm}\n  native {got}",
                row.rule
            ));
            continue;
        }
        eprintln!("ok    {:<16} wasm == native", row.rule);
    }

    let copies: usize = ROWS.iter().map(|r| r.carriers.len()).sum();
    eprintln!(
        "\nboundaries: {} rows, {copies} copies, {} failed{}",
        ROWS.len(),
        failures.len(),
        if native { "" } else { " (native skipped)" }
    );
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}
