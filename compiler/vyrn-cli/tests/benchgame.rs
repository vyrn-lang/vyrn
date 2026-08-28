//! The Benchmarks Game programs print what the game prints (RFC-0104 M1).
//!
//! The parity suite already proves the eight programs in `examples/` behave
//! identically under the interpreter, the native binary and wasm. That is a
//! statement about the three engines agreeing, and three engines can agree on a
//! wrong answer — so it is not the property this arc needs. A benchmark is only
//! a measurement of the thing it names while its output is still the game's
//! output, and the moment a program is edited for speed that is exactly what
//! stops being true silently.
//!
//! So this file compares each program's bytes against the fixture M0 committed
//! in `rfcs/bench-0104/`, whose provenance is `ref/gen.py` and the four numbers
//! the game itself publishes. It is the gate that outlives the milestone: M2
//! tunes these programs, and a tuned variant that stops printing the fixture
//! fails here rather than on a chart.
//!
//! No clang, no wasmtime, no build — the interpreter is enough to check an
//! answer — so it is not `#[ignore]`d and a plain `cargo test` runs it.
//!
//! The two stdin-reading programs use the corpus's own convention
//! (`examples/<name>.stdin`, fed by [`run_io`]), so there is one rule about
//! where a program's input comes from and not two.

mod common;
use common::*;

/// Each expressible program and the fixture its output must equal, byte for
/// byte after line-ending normalization.
///
/// ALL TEN ARE HERE NOW. regex-redux and mandelbrot were absent for the whole
/// of M1 and M2, and their absence was the milestone's boundary rather than an
/// omission: `=~` answered neither "how many" nor "where" and could not
/// substitute, and there was no byte sink for stdout or `writeFile`. Both
/// fixtures sat committed with no program beside them, which is what a named gap
/// looks like in a corpus.
///
/// RFC-0111 gave the language `writeStdout`, and `mandelbrot.vyrn` writes the
/// binary PBM the game asks for. RFC-0112 gave it `std/regex` — a searching
/// engine written in Vyrn, so the three backends run one implementation — and
/// `regexredux.vyrn` counts nine patterns and applies five substitutions.
const PROGRAMS: &[(&str, &str)] = &[
    ("nbody.vyrn", "nbody-1000.expected"),
    ("spectralnorm.vyrn", "spectralnorm-100.expected"),
    ("fannkuch.vyrn", "fannkuch-7.expected"),
    ("binarytrees.vyrn", "binarytrees-10.expected"),
    ("fasta.vyrn", "fasta-1000.expected"),
    ("revcomp.vyrn", "revcomp-1000.expected"),
    ("knucleotide.vyrn", "knucleotide-1000.expected"),
    ("pidigits.vyrn", "pidigits-27.expected"),
    ("mandelbrot.vyrn", "mandelbrot-200.expected"),
    ("regexredux.vyrn", "regexredux-1000.expected"),
];

fn fixtures_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rfcs/bench-0104")
}

#[test]
fn every_benchmark_game_program_prints_its_fixture() {
    let dir = examples_dir();
    let fixtures = fixtures_dir();
    let mut failures: Vec<String> = Vec::new();

    for (name, fixture) in PROGRAMS {
        let path = dir.join(name);
        assert!(path.exists(), "{name}: no such example");
        let expected_path = fixtures.join(fixture);
        let expected = std::fs::read(&expected_path)
            .unwrap_or_else(|e| panic!("{fixture}: {e} ({})", expected_path.display()));
        let expected = norm(&expected);

        let mut cmd = vyrn();
        cmd.arg("run").arg(&path);
        let out = run_io(cmd, &dir, &path.with_extension("stdin"));

        let err = runtime_err(&out.stderr);
        if !err.is_empty() || out.status.code() != Some(0) {
            failures.push(format!(
                "{name}: exited {:?} with stderr:\n{err}",
                out.status.code()
            ));
            continue;
        }
        if let Some(diff) = first_diff("stdout", "expected", &expected, name, &norm(&out.stdout)) {
            failures.push(format!("{name}: does not print {fixture}\n{diff}"));
        }
    }

    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

/// The stdin-fed programs really are fed, and by the fixture the FASTA
/// generator in this same corpus produces.
///
/// Without this, a lost or emptied `.stdin` fixture is not a failure anywhere:
/// `revcomp` over an empty input prints nothing, and "prints nothing" is what an
/// absent fixture would also make the expected output if someone regenerated it
/// from a broken run. The two inputs are checked against `fasta.vyrn`'s own
/// committed output instead of against themselves.
#[test]
fn the_stdin_fixtures_are_the_fasta_output() {
    let dir = examples_dir();
    let fasta = std::fs::read(fixtures_dir().join("fasta-1000.expected")).expect("read fasta");
    for name in ["revcomp", "knucleotide"] {
        let fixture = dir.join(format!("{name}.stdin"));
        let got = std::fs::read(&fixture)
            .unwrap_or_else(|e| panic!("{name}.stdin: {e} ({})", fixture.display()));
        assert_eq!(
            norm(&got),
            norm(&fasta),
            "{name}.stdin must be fasta.vyrn's census output"
        );
    }
}

/// Every committed fixture has a program that prints it.
///
/// THE FAILURE THIS EXISTS FOR: two fixtures sat in `rfcs/bench-0104/` with no
/// program beside them for the whole of M1 and M2. That was deliberate and it
/// was written down — but nothing enforced the pairing, so a THIRD one could
/// have joined them and only a reader of the RFC would have known.
///
/// The table above is the pairing. This says the table is complete: a
/// `*.expected` file that names no program in `PROGRAMS` fails here, and the
/// failure names the file.
#[test]
fn no_fixture_is_left_without_a_program() {
    let fixtures = fixtures_dir();
    let claimed: std::collections::BTreeSet<&str> =
        PROGRAMS.iter().map(|(_, fixture)| *fixture).collect();

    let mut orphans: Vec<String> = std::fs::read_dir(&fixtures)
        .expect("the fixtures directory")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".expected"))
        .filter(|n| !claimed.contains(n.as_str()))
        .collect();
    orphans.sort();

    assert!(
        orphans.is_empty(),
        "fixture(s) with no program beside them: {}. Either add the program and \
         its row above, or say in RFC-0104 which gap the fixture names.",
        orphans.join(", ")
    );
}
