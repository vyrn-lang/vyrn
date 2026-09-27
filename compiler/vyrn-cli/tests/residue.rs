//! The exit-residue ratchet.
//!
//! `VYRN_LEAK_CHECK=1` in the compiler's environment selects `std/runtime`'s
//! accounting allocator. At exit the program reports:
//!
//!   - a double free: one line on fd 2 and exit 134, at the site;
//!   - residue: `free audit: N block(s), M bytes, never freed` and exit 135;
//!   - otherwise its own exit code.
//!
//! Against `tests/pins/residue-baseline.tsv`:
//!
//!   - a double free fails, whatever the baseline says;
//!   - a `clean` row, or an example with no row, that leaks fails;
//!   - a `leak N` row that leaks more than N blocks fails;
//!   - a `leak` row that comes out smaller or clean passes with a nudge to
//!     shrink the baseline;
//!   - a `clean` row that exits nonzero fails;
//!   - an `other` row exits nonzero by design and passes while the audit is
//!     quiet.
//!
//! The audit is inside the module, so both legs report the same residue: the
//! `vyrn run` leg needs only the compiler, the wasm2c route leg needs clang,
//! wabt and simde and is skipped without them. The corpus is `route.rs`'s.

mod common;
use common::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, PartialEq, Clone)]
enum Expect {
    Clean,
    Leak(u64),
    Other,
}

fn baseline() -> HashMap<String, Expect> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/pins/residue-baseline.tsv");
    let text = std::fs::read_to_string(&path).expect("residue baseline");
    let mut out = HashMap::new();
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let mut parts = line.split('\t');
        let name = parts.next().expect("name").to_string();
        let what = parts.next().expect("verdict");
        let e = match what {
            "clean" => Expect::Clean,
            "other" => Expect::Other,
            "leak" => Expect::Leak(
                parts
                    .next()
                    .and_then(|n| n.parse().ok())
                    .expect("leak block count"),
            ),
            other => panic!("unknown baseline verdict `{other}` for `{name}`"),
        };
        out.insert(name, e);
    }
    out
}

/// The `N` of the audit's `free audit: N block(s), ...` line.
fn blocks(stderr: &str) -> Option<u64> {
    let at = stderr.rfind("free audit: ")?;
    let rest = &stderr[at + "free audit: ".len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Every example but the ones that do not build and the ones only a browser
/// can run, as `route.rs` selects.
fn corpus() -> Vec<PathBuf> {
    let dir = examples_dir();
    let mut names: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "vyrn"))
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            !KNOWN_DIVERGENT.iter().any(|(n, _)| *n == name)
                && !EXPECTED_CHECK_FAILURE.iter().any(|(n, ..)| *n == name)
                && !WASM_ONLY.iter().any(|(n, _)| *n == name)
        })
        .collect();
    names.sort();
    assert!(!names.is_empty(), "no examples found in {}", dir.display());
    names
}

fn judge(
    engine: &str,
    name: &str,
    expect: &Expect,
    code: Option<i32>,
    err: &str,
    failures: &mut Vec<String>,
    nudges: &mut usize,
) {
    match code {
        Some(134) => failures.push(format!("{name} ({engine}): DOUBLE FREE\n{err}")),
        Some(135) => {
            let got = blocks(err).unwrap_or(u64::MAX);
            match expect {
                Expect::Leak(max) if got <= *max => {
                    if got < *max {
                        *nudges += 1;
                        eprintln!(
                            "ratchet: {name} ({engine}) leaks {got} (baseline {max}) — shrink its row"
                        );
                    }
                }
                Expect::Leak(max) => failures.push(format!(
                    "{name} ({engine}): residue grew — {got} block(s), baseline allows {max}"
                )),
                _ => failures.push(format!(
                    "{name} ({engine}): new residue — {got} block(s), baseline says {expect:?}"
                )),
            }
        }
        Some(0) => {}
        Some(code) => {
            // A `leak` row that reaches here came out clean. A `clean` row
            // must exit 0: a double free through a pointer so wrong that `free`
            // reads its header out of bounds traps before `auditDeath` sees it,
            // and the exit code is the only witness (`examples/looptemp.vyrn`).
            match expect {
                Expect::Leak(_) => {
                    *nudges += 1;
                    eprintln!(
                        "ratchet: {name} ({engine}) is CLEAN now — move its row to `clean`, or \
                         to `other` if it exits {code} by design"
                    );
                }
                Expect::Clean => failures.push(format!(
                    "{name} ({engine}): exited {code}, and its row says clean — a clean row \
                     exits 0, so this is a trap or a refusal the row does not know about\n{err}"
                )),
                Expect::Other => {}
            }
        }
        None => failures.push(format!("{name} ({engine}): killed by signal")),
    }
}

#[test]
#[ignore = "the whole corpus, compiled twice; run explicitly: cargo test -p vyrn-cli --release --test residue -- --ignored"]
fn the_residue_ratchet_only_turns_one_way() {
    let base = baseline();
    let dir = examples_dir();
    let out_dir = scratch("residue");
    // A missing tool skips the route's leg, not the ratchet: the engine leg
    // measures the same module.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let route = vyrn_codegen::toolchain::wasm2c_from(&root)
        .ok()
        .flatten()
        .is_some()
        && vyrn_codegen::toolchain::simde_from(&root).is_some()
        && vyrn_codegen::toolchain::find_clang().is_some();
    if !route {
        eprintln!("SKIP the route's leg: clang, wabt or simde is missing");
    }

    // Programs run in parallel; verdicts fold in corpus order.
    let corpus = corpus();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let done = std::sync::Mutex::new(Vec::new());
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(path) = corpus.get(i) else { break };
                let v = verdict(path, &base, &dir, &out_dir, route);
                done.lock().unwrap().push((i, v));
            });
        }
    });
    let mut done = done.into_inner().unwrap();
    done.sort_by_key(|(i, _)| *i);
    let mut failures: Vec<String> = Vec::new();
    let mut nudges = 0usize;
    let (mut engine_clean, mut engine_leaks) = (0usize, 0usize);
    let (mut route_clean, mut route_leaks) = (0usize, 0usize);
    for (_, v) in done {
        failures.extend(v.failures);
        nudges += v.nudges;
        engine_clean += v.engine_clean;
        engine_leaks += v.engine_leaks;
        route_clean += v.route_clean;
        route_leaks += v.route_leaks;
    }
    if nudges > 0 {
        eprintln!("ratchet: {nudges} row(s) can tighten");
    }
    eprintln!(
        "\nresidue: engine {engine_clean} clean, {engine_leaks} leaking; \
         route {route_clean} clean, {route_leaks} leaking; {} failed",
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "the residue ratchet slipped:\n{}",
        failures.join("\n")
    );
}

/// One program's verdict on both legs.
#[derive(Default)]
struct Verdict {
    failures: Vec<String>,
    nudges: usize,
    engine_clean: usize,
    engine_leaks: usize,
    route_clean: usize,
    route_leaks: usize,
}

fn verdict(
    path: &Path,
    base: &HashMap<String, Expect>,
    dir: &Path,
    out_dir: &Path,
    route: bool,
) -> Verdict {
    let mut v = Verdict::default();
    let name = path.file_stem().unwrap().to_string_lossy().to_string();
    let expect = base.get(&name).unwrap_or(&Expect::Clean).clone();
    let stdin_fixture = path.with_extension("stdin");
    let prog_args = read_args(&path.with_extension("args"));

    // `vyrn run` compiles and runs in one process, so one variable both
    // selects the accounting allocator and arms the run.
    let mut cmd = vyrn();
    cmd.env("VYRN_LEAK_CHECK", "1");
    cmd.arg("run").arg(path).args(&prog_args);
    let r = run_io(cmd, dir, &stdin_fixture);
    let err = norm(&r.stderr);
    let before = v.failures.len();
    judge(
        "engine",
        &name,
        &expect,
        r.status.code(),
        &err,
        &mut v.failures,
        &mut v.nudges,
    );
    if r.status.code() == Some(135) {
        v.engine_leaks += 1;
    } else if v.failures.len() == before {
        v.engine_clean += 1;
    }

    if !route {
        return v;
    }
    let exe = out_dir.join(format!("{name}.exe"));
    let build = vyrn()
        .env("VYRN_LEAK_CHECK", "1")
        .arg("build")
        .arg(path)
        .arg("-o")
        .arg(&exe)
        .output()
        .expect("build");
    if !build.status.success() {
        v.failures.push(format!(
            "{name} (route): an audited build failed:\n{}{}",
            norm(&build.stdout),
            norm(&build.stderr)
        ));
        return v;
    }
    let mut cmd = Command::new(&exe);
    cmd.args(&prog_args);
    let r = run_io(cmd, dir, &stdin_fixture);
    let err = norm(&r.stderr);
    let before = v.failures.len();
    judge(
        "route",
        &name,
        &expect,
        r.status.code(),
        &err,
        &mut v.failures,
        &mut v.nudges,
    );
    if r.status.code() == Some(135) {
        v.route_leaks += 1;
    } else if v.failures.len() == before {
        v.route_clean += 1;
    }
    v
}
