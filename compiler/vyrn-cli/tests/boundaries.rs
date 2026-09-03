//! Every value-boundary check the three engines carry — RFC-0125 §3 M6, the
//! third judgment's first slice.
//!
//! §2.3 says a validated type has one producer and a boundary needs no check,
//! because the type is the proof. Before anything is deleted, this file counts
//! what exists: one row per distinct RULE, the engines that state that rule
//! THEMSELVES, and a program that makes the rule fire.
//!
//! # What a copy is
//!
//! A **carrier** is one engine's own statement of the rule — the condition, not
//! the wording. The wording has been one table since RFC-0101 M5
//! (`vyrn_frontend::trap`, and `tests/traps.rs` is what keeps it one), so what
//! this census counts is the half that is still written out per engine:
//!
//! | carrier | where |
//! |---|---|
//! | `interp` | `vyrn-frontend/src/interp.rs` |
//! | `native` | `vyrn-codegen/src/lib.rs`'s IR, and the C shim in `toolchain.rs` |
//! | `wasm` | `vyrn-codegen/src/direct.rs` |
//! | `vyrn` | a Vyrn module — `std/runtime.vyrn`, or a library that states it |
//!
//! An engine that CALLS another carrier's statement is not a carrier. The wasm
//! emitter is not a carrier of `string-utf8` because it calls `std/runtime`'s
//! `strFromBytes`; the interpreter is, because it calls Rust's `from_utf8`.
//!
//! # The two tests
//!
//! 1. [`every_boundary_row_says_the_same_thing_in_every_engine`] runs each row's
//!    program under the interpreter, the compiled wasm and the native binary and
//!    asserts byte-identical stdout, stderr and exit code. That is what makes
//!    the census's last column a fact rather than a claim: the copies agree
//!    TODAY, and a deletion slice has to keep them agreeing.
//! 2. [`the_rfc_table_lists_exactly_these_rows`] reads the census table out of
//!    `rfcs/RFC-0125-a-rule-is-stated-once.md` and refuses when it and [`ROWS`]
//!    differ. `tests/effects.rs` holds its lattice this way for the same reason:
//!    a table in prose beside a table in code is two tables.
//!
//! The native column needs clang. Without it that column is skipped by name,
//! the way every other tool-dependent tier here skips — and `VYRN_REQUIRE_TOOLS`
//! turns the skip into a failure.

mod common;
use common::*;

use std::path::{Path, PathBuf};

/// One engine's own statement of a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Carrier {
    Interp,
    Native,
    Wasm,
    Vyrn,
}

impl Carrier {
    fn key(self) -> &'static str {
        match self {
            Carrier::Interp => "interp",
            Carrier::Native => "native",
            Carrier::Wasm => "wasm",
            Carrier::Vyrn => "vyrn",
        }
    }
}

/// One row of the census: a rule, who states it, and a program that fires it.
pub struct Row {
    /// The rule's key, and the stem of its program under `tests/boundaries/`.
    pub rule: &'static str,
    /// The RFC that states the rule, as the table spells it.
    pub rfc: &'static str,
    /// Every engine that states the rule itself. `len()` is the copy count.
    pub carriers: &'static [Carrier],
}

/// Every value-boundary rule, in the order the RFC's table lists them.
///
/// The carriers were read, not grepped: a mention of a wording is not a
/// statement of a rule, and a `format!` in a comment is neither.
pub const ROWS: &[Row] = &[
    // ---- the container bounds ------------------------------------------
    Row {
        rule: "array-index",
        rfc: "RFC-0011",
        carriers: &[Carrier::Interp, Carrier::Native, Carrier::Wasm],
    },
    Row {
        rule: "string-index",
        rfc: "RFC-0022",
        carriers: &[Carrier::Interp, Carrier::Native, Carrier::Wasm],
    },
    // ---- the arithmetic boundaries -------------------------------------
    Row {
        rule: "int-div-zero",
        rfc: "RFC-0002",
        carriers: &[Carrier::Interp, Carrier::Native, Carrier::Wasm],
    },
    Row {
        rule: "int-rem-zero",
        rfc: "RFC-0002",
        carriers: &[Carrier::Interp, Carrier::Native, Carrier::Wasm],
    },
    Row {
        rule: "int-div-overflow",
        rfc: "RFC-0002",
        carriers: &[Carrier::Interp, Carrier::Native, Carrier::Wasm],
    },
    Row {
        rule: "shift-range",
        rfc: "RFC-0045",
        carriers: &[Carrier::Interp, Carrier::Native, Carrier::Wasm],
    },
    // ---- the coercions: a rule that answers rather than refusing --------
    Row {
        rule: "int-narrowing",
        rfc: "RFC-0002",
        carriers: &[Carrier::Interp, Carrier::Native, Carrier::Wasm],
    },
    Row {
        rule: "float-to-int",
        rfc: "RFC-0002",
        carriers: &[Carrier::Interp, Carrier::Native, Carrier::Wasm],
    },
    // ---- the user's own predicate --------------------------------------
    Row {
        rule: "where-scalar",
        rfc: "RFC-0003",
        carriers: &[Carrier::Interp, Carrier::Native, Carrier::Wasm],
    },
    Row {
        rule: "where-record",
        rfc: "RFC-0003",
        carriers: &[Carrier::Interp, Carrier::Native, Carrier::Wasm],
    },
    // ---- what a String may hold ----------------------------------------
    Row {
        rule: "string-nul",
        rfc: "RFC-0014",
        carriers: &[Carrier::Interp, Carrier::Native, Carrier::Vyrn],
    },
    Row {
        rule: "string-utf8",
        rfc: "RFC-0014",
        carriers: &[Carrier::Interp, Carrier::Native, Carrier::Vyrn],
    },
    // ---- the I/O boundary ----------------------------------------------
    Row {
        rule: "file-nul",
        rfc: "RFC-0014",
        carriers: &[Carrier::Interp, Carrier::Native, Carrier::Vyrn],
    },
    Row {
        rule: "file-utf8",
        rfc: "RFC-0014",
        carriers: &[Carrier::Interp, Carrier::Native, Carrier::Vyrn],
    },
    Row {
        rule: "io-status",
        rfc: "RFC-0014",
        carriers: &[Carrier::Interp, Carrier::Native, Carrier::Vyrn],
    },
    // ---- the budgets ----------------------------------------------------
    Row {
        rule: "call-depth",
        rfc: "RFC-0004",
        carriers: &[Carrier::Interp, Carrier::Native, Carrier::Wasm],
    },
    Row {
        rule: "region-depth",
        rfc: "RFC-0004",
        carriers: &[Carrier::Interp, Carrier::Native, Carrier::Wasm],
    },
    // ---- the two rules that are already stated once ---------------------
    Row {
        rule: "json-decode",
        rfc: "RFC-0018",
        carriers: &[Carrier::Vyrn],
    },
    Row {
        rule: "char-boundary",
        rfc: "RFC-0046",
        carriers: &[Carrier::Vyrn],
    },
];

/// `compiler/vyrn-cli/tests/boundaries/` — the programs and their fixtures.
fn programs_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("boundaries")
        .canonicalize()
        .expect("tests/boundaries")
}

/// The repository root, for the RFC the second test reads.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

/// One run's comparable output: stdout, stderr and exit code, CRLF-normalised.
///
/// A native binary on Windows writes `\r\n` and the interpreter does not, which
/// is a fact about the terminal and not about the rule — [`norm`] is where every
/// tier in this harness settles that, and this one settles it the same way.
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

/// The gate: every row's program answers the same thing in every engine.
#[test]
fn every_boundary_row_says_the_same_thing_in_every_engine() {
    let dir = programs_dir();
    let native = vyrn_codegen::toolchain::find_clang();
    let native = require_tools("clang", "VYRN_CLANG", native).is_some();
    if !native {
        eprintln!("SKIP the native column (no clang); interp and wasm still compared");
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
        let run = |extra: &[&str]| -> Answer {
            let mut cmd = vyrn();
            cmd.arg("run");
            cmd.args(extra);
            cmd.arg(&file);
            answer(run_io(cmd, &dir, &dir.join("nostdin")))
        };
        let interp = run(&[]);
        let wasm = run(&["--engine", "wasm"]);
        if interp != wasm {
            failures.push(format!(
                "{}: interp and wasm differ\n  interp {interp}\n  wasm   {wasm}",
                row.rule
            ));
            continue;
        }
        if !native {
            eprintln!("ok    {:<16} interp == wasm", row.rule);
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
        if interp != got {
            failures.push(format!(
                "{}: interp and native differ\n  interp {interp}\n  native {got}",
                row.rule
            ));
            continue;
        }
        eprintln!("ok    {:<16} interp == wasm == native", row.rule);
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

/// One census table, not two: the RFC's rows are [`ROWS`].
///
/// The table is found by its header line and read to the first line that is not
/// a row. Each row's first, third and fourth cells are the rule, the RFC and the
/// copy count; the fifth is the carrier list, space-separated.
#[test]
fn the_rfc_table_lists_exactly_these_rows() {
    let rfc = repo_root()
        .join("rfcs")
        .join("RFC-0125-a-rule-is-stated-once.md");
    let text = std::fs::read_to_string(&rfc).unwrap_or_else(|e| panic!("{}: {e}", rfc.display()));
    let header = "| rule | what it refuses | RFC | copies | carriers |";
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
        assert_eq!(cells.len(), 5, "row has {} cells: {line}", cells.len());
        got.push(format!(
            "{} {} {} {}",
            cells[0].trim_matches('`'),
            cells[2],
            cells[3],
            cells[4].replace('`', "")
        ));
    }
    let want: Vec<String> = ROWS
        .iter()
        .map(|r| {
            format!(
                "{} {} {} {}",
                r.rule,
                r.rfc,
                r.carriers.len(),
                r.carriers
                    .iter()
                    .map(|c| c.key())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        })
        .collect();
    assert_eq!(
        got,
        want,
        "the census table in {} and `ROWS` differ. One table, not two.",
        rfc.display()
    );

    // The count the deletion slices drive to one, stated in the RFC's prose and
    // computed here, so the sentence cannot drift from the table above it.
    let copies: usize = ROWS.iter().map(|r| r.carriers.len()).sum();
    let sentence = format!("{} rows and {copies} copies", ROWS.len());
    assert!(
        text.contains(&sentence),
        "{}: the prose should say {sentence:?}",
        rfc.display()
    );
}
