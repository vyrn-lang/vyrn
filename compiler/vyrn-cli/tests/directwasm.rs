//! The RFC-0077 burndown ladder: every example through the DIRECT wasm backend,
//! and how far it got.
//!
//!     cargo test -p vyrn-cli --release --test directwasm -- --ignored --nocapture
//!
//! M2 is ~969 emit sites and will land over several milestones. This tier is how
//! that is tracked: it builds each example with `VYRN_WASM_BACKEND=direct`, runs
//! it under wasmtime, and makes exactly the comparison `parity` makes against the
//! interpreter — same corpus, same conventions, same `common` module, so the two
//! tiers cannot disagree about what "the same run" means.
//!
//! Needs only a `wasmtime` binary. No clang, no wasi sysroot, no builtins
//! archive — which is the acceptance criterion this RFC is chasing, asserted by
//! the shape of the test rather than in prose.
//!
//! # Why a list and not a count
//!
//! [`PASSING`] is a committed list of the examples that work, and the test fails
//! if any of them stops working. A committed *count* would let the set churn
//! silently: one example starts passing while another regresses and the number is
//! unchanged. The list also IS the progress report — the diff that adds a name is
//! the milestone.
//!
//! An example that passes without being listed does not fail the run; it prints
//! a line asking to be added. Failing there would make every M2b commit that
//! widens the lowering red before it is finished, which is the opposite of what a
//! burndown wants.

mod common;
use common::*;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

/// Examples the direct backend compiles and runs identically to the
/// interpreter. Grows as M2 lands; never shrinks silently.
const PASSING: &[&str] = &[
    "arrays.vyrn",
    "autovalidate.vyrn",
    "benching.vyrn",
    "consume.vyrn",
    "ecs.vyrn",
    "enum.vyrn",
    "eventloop.vyrn",
    "externdemo2.vyrn",
    "fib.vyrn",
    "foreach.vyrn",
    "gendemo.vyrn",
    "generics.vyrn",
    "ifexpr.vyrn",
    "map.vyrn",
    "modify.vyrn",
    "ownership.vyrn",
    "protocol.vyrn",
    "record.vyrn",
    "statemod.vyrn",
    "testing.vyrn",
    "utility.vyrn",
    // The one example whose refinement is VIOLATED at runtime, so it is the one
    // that proves the checks are emitted rather than that the bytes agree: a
    // lowering that emits the type and forgets the check passes every other
    // refinement example and fails this one (RFC-0077 M2d).
    "validate_fail.vyrn",
    "validation.vyrn",
];

#[test]
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test directwasm -- --ignored --nocapture"]
fn examples_through_the_direct_wasm_backend() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime (set VYRN_WASMTIME or unpack one under <repo>/tools/)");
        return;
    };
    let dir = examples_dir();
    let out_dir = std::env::temp_dir().join("vyrn-directwasm");
    std::fs::create_dir_all(&out_dir).unwrap();

    let mut names: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "vyrn"))
        .collect();
    names.sort();

    // Grouped by what stopped it: the emitter's gap message with its line number
    // removed, so one unimplemented construct is one entry however many sites
    // hit it.
    let mut blocked: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut passed: Vec<String> = Vec::new();
    let mut considered = 0usize;
    let mut regressions: Vec<String> = Vec::new();

    for path in &names {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        // The same three exclusions parity applies: a program that never builds,
        // or whose behaviour only a browser can supply, is not a run-time
        // comparison on any backend.
        if EXPECTED_CHECK_FAILURE.iter().chain(WASM_ONLY).chain(KNOWN_DIVERGENT).any(|(n, _)| *n == name) {
            continue;
        }
        considered += 1;

        let stdin_fixture = path.with_extension("stdin");
        let prog_args = read_args(&path.with_extension("args"));

        let module = out_dir.join(format!("{name}.wasm"));
        let _ = std::fs::remove_file(&module);
        let build = vyrn()
            .arg("build")
            .arg(path)
            .arg("--target")
            .arg("wasm")
            .arg("-o")
            .arg(&module)
            .env("VYRN_WASM_BACKEND", "direct")
            .output()
            .expect("build wasm");
        if !build.status.success() {
            blocked.entry(blocker(&norm(&build.stderr))).or_default().push(name.clone());
            continue;
        }

        let mut interp_cmd = vyrn();
        interp_cmd.arg("run").arg(path).args(&prog_args);
        let interp = run_io(interp_cmd, &dir, &stdin_fixture);

        let mut wasm_cmd = Command::new(&wasmtime);
        wasm_cmd.arg("run").arg("--dir").arg(".");
        wasm_cmd.arg("--env").arg(format!("VYRN_FIXED_TIME={FIXED_TIME}"));
        wasm_cmd.arg("--env").arg(format!("VYRN_FIXED_SEED={FIXED_SEED}"));
        wasm_cmd.arg(&module).args(&prog_args);
        let w = run_io(wasm_cmd, &dir, &stdin_fixture);

        let (i_out, w_out) = (norm(&interp.stdout), norm(&w.stdout));
        let (i_err, w_err) = (runtime_err(&interp.stderr), runtime_err(&w.stderr));
        let (i_code, w_code) = (interp.status.code(), w.status.code());
        if i_out == w_out && i_err == w_err && i_code == w_code {
            passed.push(name.clone());
            if !PASSING.contains(&name.as_str()) {
                eprintln!("NEW   {name}  — passes; add it to PASSING");
            }
            continue;
        }
        // It built, so the gap is semantic rather than missing: a wrong answer
        // is a different (and worse) class of blocker than a refusal, and the
        // grouping says so.
        let detail = format!(
            "exit {i_code:?} vs {w_code:?}; stdout {i_out:?} vs {w_out:?}; \
             stderr {i_err:?} vs {w_err:?}"
        );
        blocked.entry("built, but DIVERGED from the interpreter".into()).or_default().push(name.clone());
        if PASSING.contains(&name.as_str()) {
            regressions.push(format!("{name}: {detail}"));
        } else {
            eprintln!("diff  {name}  {detail}");
        }
    }

    for name in PASSING {
        if !passed.iter().any(|p| p == name) && !regressions.iter().any(|r| r.starts_with(name)) {
            regressions.push(format!(
                "{name}: was passing, now blocked on {}",
                blocked
                    .iter()
                    .find(|(_, v)| v.iter().any(|n| n == name))
                    .map(|(k, _)| k.as_str())
                    .unwrap_or("something unreported")
            ));
        }
    }

    eprintln!("\ndirect wasm backend: {}/{considered} examples pass", passed.len());
    for (why, who) in &blocked {
        eprintln!("  {:3}  {why}", who.len());
        eprintln!("       {}", who.join(", "));
    }

    assert!(
        regressions.is_empty(),
        "examples in PASSING no longer pass:\n{}",
        regressions.join("\n")
    );
}

/// The one message this backend assembles at runtime rather than interning
/// whole, and the one no example reaches.
///
/// `error: array index 7 out of bounds` has the offending index in the MIDDLE,
/// so it is three writes and an `int_str` rather than a string constant — and a
/// bounds check that never fires reads exactly like one that fires with the
/// wrong wording. Both spellings, because the array and the string paths pick
/// different prefixes.
#[test]
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test directwasm -- --ignored"]
fn the_bounds_trap_says_what_the_interpreter_says() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = std::env::temp_dir().join("vyrn-directwasm-oob");
    std::fs::create_dir_all(&dir).unwrap();
    for (what, src) in [
        ("array", "fn main() -> Int64 {\n let xs: Array<Int64> = [1, 2, 3]\n print(xs[7])\n return 0\n}\n"),
        ("string", "fn main() -> Int64 {\n let s = \"hi\"\n let b = s[9]\n return 0\n}\n"),
    ] {
        let path = dir.join(format!("{what}.vyrn"));
        std::fs::write(&path, src).unwrap();
        let module = dir.join(format!("{what}.wasm"));
        let build = vyrn()
            .arg("build")
            .arg(&path)
            .arg("--target")
            .arg("wasm")
            .arg("-o")
            .arg(&module)
            .env("VYRN_WASM_BACKEND", "direct")
            .output()
            .expect("build wasm");
        assert!(build.status.success(), "{what}: {}", String::from_utf8_lossy(&build.stderr));

        let mut interp_cmd = vyrn();
        interp_cmd.arg("run").arg(&path);
        let interp = run_io(interp_cmd, &dir, &dir.join("no.stdin"));
        let mut wasm_cmd = Command::new(&wasmtime);
        wasm_cmd.arg("run").arg(&module);
        let w = run_io(wasm_cmd, &dir, &dir.join("no.stdin"));

        assert_eq!(runtime_err(&interp.stderr), runtime_err(&w.stderr), "{what}: stderr");
        assert_eq!(norm(&interp.stdout), norm(&w.stdout), "{what}: stdout");
        assert_eq!(interp.status.code(), w.status.code(), "{what}: exit");
        assert!(runtime_err(&w.stderr).contains(&format!("{what} index")), "{what}: wrong wording");
    }
}

/// Monomorphization discovering itself, which `generics.vyrn` does not do.
///
/// A wasm call names a function INDEX, so a specialization's index is handed out
/// where it is *discovered* and its body added later — which only works if the
/// two orders are the same. `twice` calls `wrap`, so `wrap<Int64>` is discovered
/// while three other specializations are still queued: a driver that drained its
/// worklist as a stack, or that appended out of turn, would renumber every call
/// after that point. The textual backend cannot notice — it emits symbols.
///
/// Also here because they are the shapes generics reach through: an aggregate
/// returned from a generic (the hidden leading destination, allocated before the
/// substitution is even solved), and a further generic solved from a generic
/// call's RESULT type — `firstOf(twice(41))` can only fix `A` if the call
/// reports `Pair<Int64, Int64>` rather than its record shape.
#[test]
#[ignore = "needs wasmtime; run explicitly: cargo test -p vyrn-cli --release --test directwasm -- --ignored"]
fn a_specialization_discovered_from_another_gets_the_index_its_callers_named() {
    let Some(wasmtime) = wasmtime() else {
        eprintln!("SKIP: no wasmtime");
        return;
    };
    let dir = std::env::temp_dir().join("vyrn-directwasm-mono");
    std::fs::create_dir_all(&dir).unwrap();
    let src = "\
type Pair<A, B> = { first: A, second: B }

fn wrap<T>(x: T) -> Pair<T, T> {
    return Pair { first: x, second: x }
}

fn twice<T>(x: T) -> Pair<T, T> {
    return wrap(x)
}

fn firstOf<A, B>(p: Pair<A, B>) -> A {
    return p.first
}

fn main() -> Int64 {
    let a = twice(41)
    let b = twice(\"hi\")
    print(firstOf(a) + 1)
    print(firstOf(b))
    print(wrap(true).second)
    return firstOf(a) - 41
}
";
    let path = dir.join("mono.vyrn");
    std::fs::write(&path, src).unwrap();
    let module = dir.join("mono.wasm");
    let build = vyrn()
        .arg("build")
        .arg(&path)
        .arg("--target")
        .arg("wasm")
        .arg("-o")
        .arg(&module)
        .env("VYRN_WASM_BACKEND", "direct")
        .output()
        .expect("build wasm");
    assert!(build.status.success(), "{}", String::from_utf8_lossy(&build.stderr));

    let mut interp_cmd = vyrn();
    interp_cmd.arg("run").arg(&path);
    let interp = run_io(interp_cmd, &dir, &dir.join("no.stdin"));
    let mut wasm_cmd = Command::new(&wasmtime);
    wasm_cmd.arg("run").arg(&module);
    let w = run_io(wasm_cmd, &dir, &dir.join("no.stdin"));

    // A merged specialization is the failure this is really about: `twice<Int64>`
    // and `twice<String>` are the same source and different code, and merging
    // them prints a plausible number where a string belongs.
    assert_eq!(norm(&interp.stdout), "42\nhi\ntrue\n", "the interpreter moved");
    assert_eq!(norm(&interp.stdout), norm(&w.stdout), "stdout");
    assert_eq!(runtime_err(&interp.stderr), runtime_err(&w.stderr), "stderr");
    assert_eq!(interp.status.code(), w.status.code(), "exit");
}

/// The construct a build failed on, with the source line dropped so the same gap
/// at fifty sites is one entry. Anything that is not a lowering gap is reported
/// whole — an ICE or a load error is not a burndown item.
fn blocker(stderr: &str) -> String {
    for line in stderr.lines() {
        if let Some(rest) = line.trim().strip_prefix("error: direct backend: no lowering for ") {
            return rest.rsplit_once(" at line ").map(|(what, _)| what).unwrap_or(rest).to_string();
        }
    }
    stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("(no message)").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::blocker;

    #[test]
    fn the_grouping_key_is_the_construct_not_the_site() {
        assert_eq!(
            blocker("error: direct backend: no lowering for `while` at line 12"),
            "`while`"
        );
        assert_eq!(
            blocker("error: direct backend: no lowering for `while` at line 99"),
            "`while`"
        );
        // Not a lowering gap: reported whole rather than mined for a construct.
        assert_eq!(blocker("error: cannot read foo.vyrn"), "error: cannot read foo.vyrn");
    }
}
