//! Gates that the Benchmarks Game programs print what the game prints:
//! a benchmark measures the thing it names only while its output is
//! the game's, and a program tuned for speed can stop being so silently.
//!
//! Each program's bytes are compared against its fixture in `bench/`,
//! whose provenance is `bench/gen.py` and the numbers the game publishes. The
//! stdin-reading programs use the corpus's `examples/<name>.stdin` ([`run_io`]).

mod common;
use common::*;

/// Each program and the fixture its output must equal, byte for byte after
/// line-ending normalization.
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
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench")
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

/// Without this, an emptied `.stdin` fixture fails nowhere: `revcomp` over an
/// empty input prints nothing, and a fixture regenerated from that run agrees.
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
         its row above, or delete the fixture.",
        orphans.join(", ")
    );
}
