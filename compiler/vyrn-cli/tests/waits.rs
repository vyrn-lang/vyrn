//! Waiting code runs: each waiting example built with the free audit prints its
//! recorded stdout when the host delivers every wait, and frees every frame and
//! every value whichever wait the host cancels. Each prints the same bytes when
//! the host replays what it delivered. The programs under
//! `tests/waits/` witness `vyrn test`'s clock, a race the host's delivery order
//! decides, and a waiting export's result on every host.

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
    let slow = vec![Wake {
        id: 1,
        status: 0,
        reply: None,
    }];
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

/// What `tests/waits/result.vyrn` prints on each host: `vyrn run`, the native build and, where
/// node runs, the browser shim, each with its exit code.
#[test]
fn a_waiting_export_s_result_prints_the_same_bytes_on_every_host() {
    let dir = scratch("waits-result");
    let say = |o: &std::process::Output| {
        let out = (o.status.code(), norm(&o.stdout), norm(&o.stderr));
        format!("exit {:?}\n{}{}", out.0, out.1, out.2)
    };
    let engine = vyrn()
        .args(["run", "result.vyrn"])
        .env("VYRN_LEAK_CHECK", "1")
        .current_dir(waits_dir())
        .output()
        .expect("run vyrn run");
    assert_eq!(say(&engine), "exit Some(0)\n10\nafter 3\n0.500000\n");
    let exe = dir.join("result.exe");
    let built = vyrn()
        .args(["build", "result.vyrn", "-o"])
        .arg(&exe)
        .current_dir(waits_dir())
        .output()
        .expect("run vyrn build");
    assert!(built.status.success(), "{}", norm(&built.stderr));
    let native = std::process::Command::new(&exe)
        .output()
        .expect("run the binary");
    assert_eq!(say(&native), say(&engine), "native");
    let Some(node) = node() else {
        eprintln!("NOTE: no node, so the shim's run of result.vyrn is unverified on this machine");
        return;
    };
    let wasm = dir.join("result.wasm");
    let built = vyrn()
        .args(["build", "--target", "wasm", "result.vyrn", "-o"])
        .arg(&wasm)
        .current_dir(waits_dir())
        .output()
        .expect("run vyrn build");
    assert!(built.status.success(), "{}", norm(&built.stderr));
    let web = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/wasi-min.js");
    std::fs::copy(web, dir.join("wasi-min.mjs")).expect("copy the shim");
    std::fs::write(dir.join("run.mjs"), SHIM_RUN).expect("write the runner");
    let shim = std::process::Command::new(node)
        .arg(dir.join("run.mjs"))
        .arg(&wasm)
        .output()
        .expect("node");
    assert_eq!(say(&shim), say(&engine), "shim");
}

const SHIM_RUN: &str = r#"import { readFile } from "node:fs/promises";
import { runVyrn } from "./wasi-min.mjs";

const r = await runVyrn(await readFile(process.argv[2]), {});
process.stdout.write(r.stdout);
process.stderr.write(r.stderr);
process.exitCode = r.exitCode;
"#;

/// A host that calls a waiting export reads the result its task returned, once the task
/// ended, and learns of a task it cancelled, which returned nothing.
#[test]
fn the_host_reads_the_result_of_a_task_it_started() {
    let dir = scratch("waits-host");
    let wasm = dir.join("result.wasm");
    let out = vyrn()
        .args(["build", "--target", "wasm", "result.vyrn", "-o"])
        .arg(&wasm)
        .current_dir(waits_dir())
        .output()
        .expect("run vyrn build");
    assert!(out.status.success(), "{}", norm(&out.stderr));
    let module = std::fs::read(&wasm).expect("the built module");
    let (mut res, code) =
        vyrn_cli::wasmrun::start(&module, &quiet("result.vyrn", None), None).expect("start");
    assert_eq!(code, 0);
    assert_eq!(res.ask_int("later", &[4]), Ok(8));
    assert_eq!(res.ask_text("named", &[2]).as_deref(), Ok("after 2"));
    // `main` waits on operations 1 to 3, so the export's sleep is operation 4.
    let mut cancel: Vec<Wake> = (1..=3)
        .map(|id| Wake {
            id,
            status: 0,
            reply: None,
        })
        .collect();
    cancel.push(Wake {
        id: 4,
        status: 1,
        reply: None,
    });
    let (mut res, _) = vyrn_cli::wasmrun::start(&module, &quiet("result.vyrn", Some(cancel)), None)
        .expect("start");
    let got = res.ask_text("named", &[2]);
    assert!(
        got.as_ref().is_err_and(|e| e.contains("was cancelled")),
        "{got:?}"
    );
}
