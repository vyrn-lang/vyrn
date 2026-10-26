//! Waiting code runs: each waiting example built with the free audit prints its
//! recorded stdout when the host delivers every wait, and frees every frame and
//! every value whichever wait the host cancels. Each prints the same bytes when
//! the host replays what it delivered. The programs under
//! `tests/waits/` witness `vyrn test`'s clock, a race the host's delivery order
//! decides, and what does not build.

mod common;
use common::*;
use vyrn_cli::wasmrun::{run, Run, Wake};

fn waits_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/waits")
}

/// The module the example `name` builds to, with the free audit.
fn audited(name: &str) -> Vec<u8> {
    let dir = scratch("waits");
    let wasm = dir.join("audited.wasm");
    let out = vyrn()
        .args(["build", "--target", "wasm", name, "-o"])
        .arg(&wasm)
        .env("VYRN_LEAK_CHECK", "1")
        .current_dir(examples_dir())
        .output()
        .expect("run vyrn build");
    assert!(out.status.success(), "{}", norm(&out.stderr));
    std::fs::read(&wasm).expect("the built module")
}

fn quiet(name: &str, replay: Option<Vec<Wake>>) -> Run {
    Run {
        argv: vec![name.to_string()],
        capture_stdout: true,
        capture_stderr: true,
        virtual_clock: true,
        replay,
        ..Default::default()
    }
}

#[test]
fn a_waiting_program_resumes_at_each_await() {
    for name in &waiting() {
        let bytes = audited(name);
        let out = run(&bytes, quiet(name, None)).expect("the module runs");
        let stem = name.trim_end_matches(".vyrn");
        let want = std::fs::read_to_string(examples_dir().join(format!("expected/{stem}.stdout")));
        assert_eq!(norm(&out.stdout), want.unwrap(), "{name}");
        assert_eq!((out.code, norm(&out.stderr)), (0, String::new()), "{name}");
    }
}

/// Each cancellation ends `main` at that wait with exit 1, after the output
/// before it, and the free audit finds nothing left: every frame on the path,
/// every branch a fork cancelled with it, and every value they held took the
/// cancellation edge.
#[test]
fn a_cancelled_wait_releases_every_frame_on_its_path() {
    for name in &waiting() {
        let bytes = audited(name);
        let full = run(&bytes, quiet(name, None)).expect("the module runs");
        let all = norm(&full.stdout);
        for k in 0..full.wakes.len() {
            let mut wakes = full.wakes[..=k].to_vec();
            wakes[k].status = 1;
            let out = run(&bytes, quiet(name, Some(wakes))).expect("the module runs");
            let printed = norm(&out.stdout);
            assert!(all.starts_with(&printed), "{name} wake {k}: {printed}");
            assert_eq!(
                (out.code, norm(&out.stderr)),
                (1, String::new()),
                "{name} wake {k}"
            );
        }
    }
}

/// The replay gate: each waiting example, run on the wall clock with what the host
/// delivered recorded, prints the same bytes and exits the same when the host
/// replays it.
#[test]
fn a_replay_of_the_recorded_wakes_prints_the_same_bytes() {
    for name in &waiting() {
        let bytes = audited(name);
        let wall = Run {
            virtual_clock: false,
            ..quiet(name, None)
        };
        let first = run(&bytes, wall).expect("the module runs");
        let again = run(&bytes, quiet(name, Some(first.wakes.clone()))).expect("the module runs");
        let seen = |o: &vyrn_cli::wasmrun::Outcome| (o.code, o.stdout.clone(), o.stderr.clone());
        assert_eq!(seen(&again), seen(&first), "{name}");
        assert_eq!(again.wakes, first.wakes, "{name}");
    }
}

/// A replay is a schedule the clock need not produce: delivered first, the slow timer wins
/// the race, the fast branch is cancelled where it waits, and its timer is never delivered.
#[test]
fn the_wake_a_host_delivers_first_wins_a_race() {
    let dir = scratch("waits-race");
    let wasm = dir.join("race.wasm");
    let out = vyrn()
        .args(["build", "--target", "wasm", "race.vyrn", "-o"])
        .arg(&wasm)
        .env("VYRN_LEAK_CHECK", "1")
        .current_dir(waits_dir())
        .output()
        .expect("run vyrn build");
    assert!(out.status.success(), "{}", norm(&out.stderr));
    let bytes = std::fs::read(&wasm).expect("the built module");
    let clock = run(&bytes, quiet("race.vyrn", None)).expect("the module runs");
    assert_eq!(norm(&clock.stdout), "second: fast after 10, held by fast\n");
    let slow = vec![Wake { id: 1, status: 0 }];
    for _ in 0..2 {
        let out = run(&bytes, quiet("race.vyrn", Some(slow.clone()))).expect("the module runs");
        let seen = (out.code, norm(&out.stdout), norm(&out.stderr), out.wakes);
        let want = "first: slow after 50, held by slow\n".to_string();
        assert_eq!(seen, (0, want, String::new(), slow.clone()));
    }
}

/// A test that sleeps an hour passes at once: its timers run on a clock that only
/// a wake advances.
#[test]
fn a_test_sleeps_on_the_virtual_clock() {
    let begun = std::time::Instant::now();
    let out = vyrn()
        .args(["test", "longsleep.vyrn"])
        .current_dir(waits_dir())
        .output()
        .expect("run vyrn test");
    let report = "test \"an hour's sleep\" ... ok\n\n1 passed, 0 failed\n";
    assert_eq!(
        (norm(&out.stdout), norm(&out.stderr)),
        (report.to_string(), String::new())
    );
    assert!(begun.elapsed() < std::time::Duration::from_secs(60));
}

/// A waiting lambda a specialization binds is a direct call with its own frame.
#[test]
fn a_waiting_lambda_runs_where_its_parameter_is_bound() {
    let out = vyrn()
        .args(["run", "lambda.vyrn"])
        .env("VYRN_LEAK_CHECK", "1")
        .current_dir(waits_dir())
        .output()
        .expect("run vyrn run");
    let seen = (out.status.code(), norm(&out.stdout), norm(&out.stderr));
    assert_eq!(seen, (Some(0), "3\n".to_string(), String::new()));
}

/// The build refuses what has no lowering yet, naming the function and the line.
#[test]
fn a_waiting_export_with_a_result_does_not_build_yet() {
    let dir = scratch("waits-refused");
    let out = vyrn()
        .args(["build", "--target", "wasm", "result.vyrn", "-o"])
        .arg(dir.join("w.wasm"))
        .current_dir(waits_dir())
        .output()
        .expect("run vyrn build");
    assert!(!out.status.success());
    let want = "error: waiting code does not build yet: `later` waits at line 5\n";
    assert_eq!(norm(&out.stderr), want);
}
