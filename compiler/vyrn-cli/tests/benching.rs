//! `vyrn bench`. The `--check` face is deterministic, needs no clang,
//! and is pinned byte for byte. The native timing face needs clang, so its tests
//! are `#[ignore]`d and assert the report's shape, never the numbers.

use std::path::PathBuf;
use std::process::Command;

fn vyrn() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vyrn"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("vyrn-benching-tests").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn norm(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace("\r\n", "\n")
}

#[test]
fn check_runs_each_body_once_with_exact_output() {
    let dir = scratch("check-mixed");
    let file = dir.join("b.vyrn");
    std::fs::write(
        &file,
        "bench \"ok one\" {\n\
         \x20   blackBox(1 + 1)\n\
         }\n\
         bench \"traps\" {\n\
         \x20   let mut xs: Array<Int64> = []\n\
         \x20   blackBox(xs[0])\n\
         }\n\
         bench \"ok two\" {\n\
         \x20   blackBox(2)\n\
         }\n",
    )
    .unwrap();
    let out = vyrn()
        .arg("bench")
        .arg(&file)
        .arg("--check")
        .output()
        .unwrap();
    // A trapping bench exits 1, but the run continues to the next bench.
    assert_eq!(out.status.code(), Some(1));
    let stdout = norm(&out.stdout);
    let expected = "bench \"ok one\" ... ok\n\
                    bench \"traps\" ... FAILED: array index 0 out of bounds\n\
                    bench \"ok two\" ... ok\n\
                    \n\
                    2 ok, 1 failed\n";
    assert_eq!(stdout, expected, "got:\n{stdout}");
}

#[test]
fn check_name_filter_selects_a_subset() {
    let dir = scratch("check-filter");
    let file = dir.join("b.vyrn");
    std::fs::write(
        &file,
        "bench \"alpha\" { blackBox(1) }\n\
         bench \"beta\" { blackBox(2) }\n\
         bench \"alphabet\" { blackBox(3) }\n",
    )
    .unwrap();
    let out = vyrn()
        .arg("bench")
        .arg(&file)
        .args(["--check", "--name", "alpha"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        norm(&out.stdout),
        "bench \"alpha\" ... ok\nbench \"alphabet\" ... ok\n\n2 ok, 0 failed\n"
    );
}

#[test]
fn no_benches_prints_no_benches_and_exits_zero() {
    let dir = scratch("check-none");
    let file = dir.join("b.vyrn");
    std::fs::write(&file, "fn main() -> Int64 { return 0 }\n").unwrap();
    let out = vyrn()
        .arg("bench")
        .arg(&file)
        .arg("--check")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(norm(&out.stdout), "no benches\n");
}

#[test]
fn blackbox_outside_a_bench_or_test_is_a_checker_error() {
    let dir = scratch("bb-outside");
    let file = dir.join("b.vyrn");
    std::fs::write(
        &file,
        "fn main() -> Int64 { let x = blackBox(1) return x }\n",
    )
    .unwrap();
    let out = vyrn().arg("check").arg(&file).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = norm(&out.stderr);
    assert!(
        err.contains("`blackBox` is only available inside a `bench` or `test` block"),
        "got:\n{err}"
    );
}

#[test]
fn blackbox_inside_bench_and_test_is_accepted() {
    let dir = scratch("bb-inside");
    let file = dir.join("b.vyrn");
    std::fs::write(
        &file,
        "bench \"b\" { blackBox(1) }\n\
         test \"t\" { assertEq(blackBox(2), 2) }\n\
         fn main() -> Int64 { return 0 }\n",
    )
    .unwrap();
    let out = vyrn().arg("check").arg(&file).output().unwrap();
    assert!(out.status.success(), "stderr:\n{}", norm(&out.stderr));
    assert_eq!(norm(&out.stdout), "ok\n");
}

#[test]
fn a_branch_that_yields_blackbox_runs_under_both_engines() {
    // The direct backend lowers `blackBox(v)` as `v` and needs a type row for
    // the name, or an `if` arm yielding one is refused where the interpreter runs
    // it.
    let dir = scratch("bb-branch");
    let file = dir.join("b.vyrn");
    std::fs::write(
        &file,
        "bench \"a branch that yields blackBox\" {\n\
         \x20   let n = if true { blackBox(3) } else { 0 }\n\
         \x20   blackBox(n)\n\
         }\n",
    )
    .unwrap();
    let out = vyrn()
        .arg("bench")
        .arg(&file)
        .arg("--check")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr:
{}",
        norm(&out.stderr)
    );
}

#[test]
fn bench_bodies_are_stripped_from_the_emitted_module() {
    let dir = scratch("strip");
    let file = dir.join("b.vyrn");
    std::fs::write(
        &file,
        "bench \"UNIQUE_BENCH_MARKER\" { let s = \"SECRET_IN_BENCH_BODY\" blackBox(s.byteLength) }\n\
         fn main() -> Int64 { print(1) return 0 }\n",
    )
    .unwrap();
    let out = vyrn().arg("emit-wat").arg(&file).output().unwrap();
    assert!(out.status.success(), "{}", norm(&out.stderr));
    let ir = norm(&out.stdout);
    assert!(
        !ir.contains("SECRET_IN_BENCH_BODY"),
        "bench string leaked into the module"
    );
    assert!(
        !ir.contains("UNIQUE_BENCH_MARKER"),
        "bench name leaked into the module"
    );
    // No optimizer barrier leaks into an ordinary compile: `blackBox` reserves
    // sixteen bytes of linear memory per site. The other half, that the barrier
    // is there in the harness, is `native_bench_reports_the_expected_shape`.
    let plain_file = dir.join("plain.vyrn");
    std::fs::write(&plain_file, "fn main() -> Int64 { print(1) return 0 }\n").unwrap();
    let plain = vyrn().arg("emit-wat").arg(&plain_file).output().unwrap();
    assert_eq!(
        ir,
        norm(&plain.stdout),
        "the bench file's module must be the plain file's, byte for byte"
    );
}

#[test]
fn a_file_may_have_both_benches_and_a_main() {
    let dir = scratch("both");
    let file = dir.join("b.vyrn");
    std::fs::write(
        &file,
        "bench \"b\" { blackBox(6 * 7) }\n\
         fn main() -> Int64 { print(99) return 0 }\n",
    )
    .unwrap();
    let run = vyrn().arg("run").arg(&file).output().unwrap();
    assert!(run.status.success());
    assert_eq!(norm(&run.stdout).trim(), "99");
    let bench = vyrn()
        .arg("bench")
        .arg(&file)
        .arg("--check")
        .output()
        .unwrap();
    assert!(bench.status.success());
    assert_eq!(
        norm(&bench.stdout),
        "bench \"b\" ... ok\n\n1 ok, 0 failed\n"
    );
}

#[test]
#[ignore = "needs clang; run explicitly: cargo test -p vyrn-cli --test benching -- --ignored"]
fn native_bench_reports_the_expected_shape() {
    let dir = scratch("native");
    let file = dir.join("b.vyrn");
    std::fs::write(
        &file,
        "fn hashTo(n: Int64) -> Int64 {\n\
         \x20   let mut h = 0\n\
         \x20   let mut i = 0\n\
         \x20   while i < n {\n\
         \x20       h = (h * 31 + i) % 1000000007\n\
         \x20       i = i + 1\n\
         \x20   }\n\
         \x20   return h\n\
         }\n\
         bench \"hash\" { blackBox(hashTo(blackBox(200))) }\n\
         bench \"push\" { let mut xs: Array<Int64> = [] let mut i = 0 while i < 200 { xs.push(i) i = i + 1 } blackBox(xs.length) }\n\
         fn main() -> Int64 { return 0 }\n",
    )
    .unwrap();
    let out = vyrn().arg("bench").arg(&file).output().unwrap();
    assert!(out.status.success(), "stderr:\n{}", norm(&out.stderr));
    let stdout = norm(&out.stdout);
    // `bench "name"   min <num> <unit>   median ...   mean ...   (N samples x M iters)`.
    let line = regex_like(&stdout, "bench \"hash\"");
    assert!(line.is_some(), "missing hash line:\n{stdout}");
    for name in ["hash", "push"] {
        let l = regex_like(&stdout, &format!("bench \"{name}\"")).unwrap();
        assert!(l.contains(" min "), "no min column: {l}");
        assert!(l.contains(" median "), "no median column: {l}");
        assert!(l.contains(" mean "), "no mean column: {l}");
        assert!(
            l.contains(" ns") || l.contains(" µs") || l.contains(" ms") || l.contains(" s "),
            "no time unit suffix: {l}"
        );
        assert!(
            l.contains(" samples × ") && l.contains(" iters)"),
            "no sample/iter counts: {l}"
        );
    }
    assert!(
        stdout.contains("\n2 benches\n"),
        "missing footer:\n{stdout}"
    );
    // The native route runs the module through wasm2c and clang at `-O2`, which
    // folds the loop away if `blackBox` is the identity. A floor, not a number:
    // two hundred rounds of a multiply, an add and a modulo cannot take 50 ns,
    // and a folded loop cannot take more.
    let hash = regex_like(&stdout, "bench \"hash\"").unwrap();
    let min = hash.split(" min ").nth(1).expect("a min column");
    let (value, rest) = min.split_once(' ').expect("a value and a unit");
    let ns: f64 = value.parse::<f64>().expect("a number")
        * match rest.split_whitespace().next().unwrap_or("") {
            "ns" => 1.0,
            "\u{b5}s" => 1_000.0,
            "ms" => 1_000_000.0,
            other => panic!("unknown unit {other:?} in {hash}"),
        };
    assert!(
        ns >= 50.0,
        "the `blackBox` barrier is gone - the optimizer folded the loop: {hash}"
    );
}

#[test]
#[ignore = "needs clang; run explicitly: cargo test -p vyrn-cli --test benching -- --ignored"]
fn a_root_function_does_not_replace_a_std_module_private_of_the_same_name() {
    // The harness formats its timings with `std/bench`'s private `twoDecimals`.
    // The loader's name-privacy rename keeps a root function of the
    // same name from replacing it.
    let dir = scratch("private-collision");
    let file = dir.join("b.vyrn");
    std::fs::write(
        &file,
        "fn twoDecimals(value: Int64, unit: Int64) -> String {
             return \"XX\"
         }
         fn hashTo(n: Int64) -> Int64 {
             let mut h = 0
             let mut i = 0
             while i < n {
                 h = (h * 31 + i) % 1000000007
                 i = i + 1
             }
             return h
         }
         bench \"slow\" { blackBox(hashTo(blackBox(20000))) }
         fn main() -> Int64 { print(twoDecimals(1, 1)) return 0 }
",
    )
    .unwrap();
    let out = vyrn().arg("bench").arg(&file).output().unwrap();
    assert!(
        out.status.success(),
        "stderr:
{}",
        norm(&out.stderr)
    );
    let stdout = norm(&out.stdout);
    let l = regex_like(&stdout, "bench \"slow\"").expect(&stdout);
    assert!(
        !l.contains("XX"),
        "the root `twoDecimals` formatted the report: {l}"
    );
    // 20000 rounds take microseconds on any machine, so the report goes through
    // `twoDecimals`, not the bare-`ns` branch. If this fails, the assertion above
    // proves nothing.
    assert!(
        l.contains(" µs") || l.contains(" ms"),
        "bench too fast to exercise the formatter: {l}"
    );
}

#[test]
#[ignore = "needs clang; run explicitly: cargo test -p vyrn-cli --test benching -- --ignored"]
fn a_root_private_may_share_a_name_with_an_injected_module_private() {
    // `cur` and `step` are private to `std/jsonread`, which the harness pulls in.
    // A root `cur` over a different record shape must not be coerced to theirs.
    let dir = scratch("loud-collision");
    let file = dir.join("b.vyrn");
    std::fs::write(
        &file,
        "type MyP = { toks: Array<Int64>, n: Int64 }
         fn cur(p: MyP) -> Int64 { return p.n }
         fn step(p: MyP) -> Int64 { return p.toks.length }
         bench \"t\" {
             let a = MyP { toks: [1, 2], n: 2 }
             blackBox(cur(a) + step(a))
         }
         fn main() -> Int64 { return 0 }
",
    )
    .unwrap();
    let out = vyrn().arg("bench").arg(&file).output().unwrap();
    assert!(
        out.status.success(),
        "stderr:
{}",
        norm(&out.stderr)
    );
    assert!(
        norm(&out.stdout).contains(
            "
1 benches
"
        ),
        "stdout:
{}",
        norm(&out.stdout)
    );
}

/// Every private function name of every module the bench harness pulls in, at
/// once, declared by a program that never imports any of them. The two tests
/// above are single instances of this class: a clash either fails naming a type
/// the program never saw, or compiles and calls the wrong body.
///
/// The list is read off the source, so it grows when the harness grows.
#[test]
#[ignore = "needs clang; run explicitly: cargo test -p vyrn-cli --test benching -- --ignored"]
fn no_private_name_of_an_injected_module_is_reserved() {
    let mut names: Vec<String> = Vec::new();
    for module in ["bench", "time", "json", "jsonread"] {
        let src = std::fs::read_to_string(repo_root().join(format!("std/{module}.vyrn")))
            .unwrap_or_else(|e| panic!("cannot read std/{module}.vyrn: {e}"));
        for line in src.lines() {
            // A private declaration is `fn name(` at column zero; `export fn`
            // and `gen fn` do not start with `fn `.
            let Some(rest) = line.strip_prefix("fn ") else {
                continue;
            };
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                names.push(name);
            }
        }
    }
    names.sort();
    names.dedup();
    assert!(
        names.len() > 20,
        "the private-name scan found only {}: the shape of `std/` changed and \
         this test is no longer reading it",
        names.len()
    );

    let mut src = String::new();
    for (i, n) in names.iter().enumerate() {
        src.push_str(&format!("fn {n}() -> Int64 {{ return {i} }}\n"));
    }
    let calls: Vec<String> = names.iter().map(|n| format!("{n}()")).collect();
    src.push_str(&format!(
        "\nfn all() -> Int64 {{ return {} }}\n\nbench \"t\" {{\n\
         \x20   blackBox(all())\n\
         }}\n\
         fn main() -> Int64 {{ print(all()) return 0 }}\n",
        calls.join(" + ")
    ));

    let dir = scratch("injected-names");
    let file = dir.join("b.vyrn");
    std::fs::write(&file, &src).unwrap();
    let out = vyrn().arg("bench").arg(&file).output().unwrap();
    assert!(
        out.status.success(),
        "{} of these names is reserved under `vyrn bench`:\n{}\nstderr:\n{}",
        names.len(),
        names.join(", "),
        norm(&out.stderr)
    );
    assert!(
        norm(&out.stdout).contains("\n1 benches\n"),
        "stdout:\n{}",
        norm(&out.stdout)
    );
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// The first line of `text` that starts with `needle`.
fn regex_like<'a>(text: &'a str, needle: &str) -> Option<&'a str> {
    text.lines().find(|l| l.starts_with(needle))
}

fn examples_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("examples")
}

#[test]
fn bench_corpus_is_exactly_the_bench_bearing_examples() {
    // CI's blocking `--check` step scans `examples/*.vyrn` for `bench "`, so a
    // bench-bearing example added or lost surfaces as a change here.
    let mut found: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(examples_dir()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("vyrn") {
            continue;
        }
        let src = std::fs::read_to_string(&path).unwrap();
        if src.contains("bench \"") {
            found.push(path.file_stem().unwrap().to_string_lossy().into_owned());
        }
    }
    found.sort();
    assert_eq!(
        found,
        vec![
            "benching".to_string(),
            "binarytrees".to_string(),
            "contractquery".to_string(),
            "fannkuch".to_string(),
            "fasta".to_string(),
            "jsonplace".to_string(),
            "knucleotide".to_string(),
            "langbench".to_string(),
            "membench".to_string(),
            "namedplace".to_string(),
            "nbody".to_string(),
            "pidigits".to_string(),
            "revcomp".to_string(),
            "simdbench".to_string(),
            "smallarray".to_string(),
            "spectralnorm".to_string(),
            "tryplace".to_string()
        ],
        "bench corpus drifted"
    );
}

/// `--compare` matches a baseline entry by name alone, and CI merges the
/// per-example `--json` reports into one `bench/baseline.json`. A shared name
/// would silently compare one bench against the other's timing.
#[test]
fn no_two_benches_in_the_corpus_share_a_name() {
    let mut seen: Vec<(String, String)> = Vec::new(); // (bench name, file stem)
    let mut clashes: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(examples_dir()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("vyrn") {
            continue;
        }
        let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
        for line in std::fs::read_to_string(&path).unwrap().lines() {
            let Some(rest) = line.trim_start().strip_prefix("bench \"") else {
                continue;
            };
            let Some(name) = rest.split('"').next() else {
                continue;
            };
            if let Some((_, other)) = seen.iter().find(|(n, _)| n == name) {
                clashes.push(format!("`{name}` is in both {other}.vyrn and {stem}.vyrn"));
            }
            seen.push((name.to_string(), stem.clone()));
        }
    }
    assert!(
        clashes.is_empty(),
        "bench names must be unique across the corpus — the merged baseline keys \
         on the name alone:\n  {}",
        clashes.join("\n  ")
    );
    assert!(seen.len() > 20, "expected the whole corpus, found {seen:?}");
}

#[test]
#[ignore = "needs clang; run explicitly: cargo test -p vyrn-cli --test benching -- --ignored"]
fn json_report_parses_and_is_stable_ordered() {
    // Asserts the schema and declaration order, never the timing numbers.
    let dir = scratch("json");
    let file = dir.join("b.vyrn");
    std::fs::write(
        &file,
        "bench \"zeta\" { blackBox(1 + 1) }\n\
         bench \"alpha\" { blackBox(2 + 2) }\n\
         fn main() -> Int64 { return 0 }\n",
    )
    .unwrap();
    let out = vyrn()
        .arg("bench")
        .arg(&file)
        .arg("--json")
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr:\n{}", norm(&out.stderr));
    let stdout = norm(&out.stdout);
    let doc = vyrn_frontend::schema::parse_json(stdout.trim()).expect("report is valid JSON");
    assert!(
        matches!(doc.get("backend"), Some(vyrn_frontend::schema::Json::Str(s)) if s == "native")
    );
    assert!(matches!(doc.get("opt"), Some(vyrn_frontend::schema::Json::Str(s)) if s == "O2"));
    let benches = match doc.get("benches") {
        Some(vyrn_frontend::schema::Json::Arr(a)) => a,
        _ => panic!("no benches array in:\n{stdout}"),
    };
    let names: Vec<String> = benches
        .iter()
        .map(|b| match b.get("name") {
            Some(vyrn_frontend::schema::Json::Str(s)) => s.clone(),
            _ => panic!("bench entry has no name"),
        })
        .collect();
    assert_eq!(names, vec!["zeta".to_string(), "alpha".to_string()]);
    for b in benches {
        for key in ["minNs", "medianNs", "meanNs", "samples", "iters"] {
            match b.get(key) {
                Some(vyrn_frontend::schema::Json::Num(n)) => {
                    assert_eq!(n.fract(), 0.0, "{key} not integer")
                }
                _ => panic!("bench entry missing {key}"),
            }
        }
    }
}

#[test]
#[ignore = "needs clang; run explicitly: cargo test -p vyrn-cli --test benching -- --ignored"]
fn compare_against_a_placeholder_baseline_is_all_new_exit_zero() {
    let dir = scratch("compare-placeholder");
    let file = dir.join("b.vyrn");
    std::fs::write(
        &file,
        "bench \"a\" { blackBox(1 + 1) }\nfn main() -> Int64 { return 0 }\n",
    )
    .unwrap();
    let baseline = dir.join("baseline.json");
    std::fs::write(&baseline, "{\"placeholder\":true,\"benches\":[]}\n").unwrap();
    let out = vyrn()
        .arg("bench")
        .arg(&file)
        .args(["--compare"])
        .arg(&baseline)
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr:\n{}", norm(&out.stderr));
    let stdout = norm(&out.stdout);
    assert!(
        stdout.contains("bench \"a\" ... new"),
        "expected `new`, got:\n{stdout}"
    );
    assert!(stdout.contains("no regressions"), "got:\n{stdout}");
}

#[test]
#[ignore = "needs clang; run explicitly: cargo test -p vyrn-cli --test benching -- --ignored"]
fn compare_flags_a_regression_against_a_tiny_baseline() {
    // A baseline min of 1 ns is impossibly fast, so any real run regresses.
    // Asserts the verdict and exit code, not the factor.
    let dir = scratch("compare-regress");
    let file = dir.join("b.vyrn");
    // Data-dependent work, so the min is reliably above 1 ns; a trivial
    // `blackBox(1+1)` folds to about 0 ns and would compare as `ok`.
    std::fs::write(
        &file,
        "bench \"a\" { let mut xs: Array<Int64> = [] let mut i = 0 while i < 500 { xs.push(i) i = i + 1 } blackBox(xs.length) }\n\
         fn main() -> Int64 { return 0 }\n",
    )
    .unwrap();
    let baseline = dir.join("baseline.json");
    std::fs::write(
        &baseline,
        "{\"backend\":\"native\",\"opt\":\"O2\",\"benches\":[{\"name\":\"a\",\"minNs\":1,\"medianNs\":1,\"meanNs\":1,\"samples\":1,\"iters\":1}]}\n",
    )
    .unwrap();
    let out = vyrn()
        .arg("bench")
        .arg(&file)
        .args(["--compare"])
        .arg(&baseline)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "stderr:\n{}", norm(&out.stderr));
    let stdout = norm(&out.stdout);
    assert!(
        stdout.contains("bench \"a\" ... REGRESSED x"),
        "got:\n{stdout}"
    );
    assert!(stdout.contains("1 regressed"), "got:\n{stdout}");
}

#[test]
fn check_rejects_json_and_compare_flags() {
    // The guard fires before any compile, so no clang is needed.
    let dir = scratch("mutex");
    let file = dir.join("b.vyrn");
    std::fs::write(
        &file,
        "bench \"a\" { blackBox(1) }\nfn main() -> Int64 { return 0 }\n",
    )
    .unwrap();
    let out = vyrn()
        .arg("bench")
        .arg(&file)
        .args(["--check", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(norm(&out.stderr).contains("--check cannot be combined with --json or --compare"));
}

/// The `--check` face lowers a body through `bodies_wasm`; the timing face lifts
/// each body into a function the native harness calls, so the emitter sees
/// different trees on the two faces.
#[test]
#[ignore = "needs clang; run explicitly: cargo test -p vyrn-cli --test benching -- --ignored"]
fn every_bench_program_times_on_the_native_route() {
    fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "vyrn")
                && std::fs::read_to_string(&p).unwrap().contains("bench \"")
            {
                out.push(p);
            }
        }
    }
    let examples = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let mut corpus = Vec::new();
    walk(&examples, &mut corpus);
    corpus.sort();
    assert!(
        !corpus.is_empty(),
        "no bench program under {}",
        examples.display()
    );
    let mut failed = Vec::new();
    for f in &corpus {
        // No other gate reaches a bench body. Under the audit, a leak or a double
        // free exits 135 or 134 here instead of running out of memory on CI.
        let out = vyrn()
            .arg("bench")
            .arg(f)
            .arg("--json")
            .env("VYRN_LEAK_CHECK", "1")
            .output()
            .unwrap();
        if !out.status.success() {
            failed.push(format!("{}:\n{}", f.display(), norm(&out.stderr)));
        }
    }
    assert!(
        failed.is_empty(),
        "{} of {} bench programs failed on the timing face:\n{}",
        failed.len(),
        corpus.len(),
        failed.join("\n")
    );
}
