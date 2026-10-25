//! Waiting code runs: `tests/waits/frames.vyrn` built with the free audit prints
//! its `frames.stdout` when the host delivers every wait, and frees every frame
//! and every value whichever wait the host cancels. `lambda.vyrn` and
//! `region.vyrn` witness what does not build.

mod common;
use common::*;
use vyrn_cli::wasmrun::{run, Run, Wake};

fn waits_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/waits")
}

/// The module `name` builds to, with the free audit.
fn audited(name: &str) -> Vec<u8> {
    let dir = scratch("waits");
    let wasm = dir.join("audited.wasm");
    let out = vyrn()
        .args(["build", "--target", "wasm", name, "-o"])
        .arg(&wasm)
        .env("VYRN_LEAK_CHECK", "1")
        .current_dir(waits_dir())
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
    let bytes = audited("frames.vyrn");
    let out = run(&bytes, quiet("frames.vyrn", None)).expect("the module runs");
    let want = std::fs::read_to_string(waits_dir().join("frames.stdout")).unwrap();
    assert_eq!(norm(&out.stdout), want);
    assert_eq!((out.code, norm(&out.stderr)), (0, String::new()));
    assert_eq!(out.wakes.len(), 9);
}

/// Each cancellation ends `main` at that wait with exit 1, after the output
/// before it, and the free audit finds nothing left: every frame on the path and
/// every value it held took the cancellation edge.
#[test]
fn a_cancelled_wait_releases_every_frame_on_its_path() {
    let bytes = audited("frames.vyrn");
    let full = run(&bytes, quiet("frames.vyrn", None)).expect("the module runs");
    let all = norm(&full.stdout);
    for k in 0..full.wakes.len() {
        let mut wakes = full.wakes[..=k].to_vec();
        wakes[k].status = 1;
        let out = run(&bytes, quiet("frames.vyrn", Some(wakes))).expect("the module runs");
        let printed = norm(&out.stdout);
        assert!(all.starts_with(&printed), "wake {k}: {printed}");
        assert_eq!(
            (out.code, norm(&out.stderr)),
            (1, String::new()),
            "wake {k}"
        );
    }
}

/// The build refuses what has no lowering yet, naming the function and the line.
#[test]
fn a_waiting_lambda_and_a_waiting_region_do_not_build_yet() {
    for (name, line) in [("lambda", 10), ("region", 8)] {
        let dir = scratch("waits-refused");
        let out = vyrn()
            .args(["build", "--target", "wasm", &format!("{name}.vyrn"), "-o"])
            .arg(dir.join("w.wasm"))
            .current_dir(waits_dir())
            .output()
            .expect("run vyrn build");
        assert!(!out.status.success(), "{name}");
        let want = format!("error: waiting code does not build yet: `main` waits at line {line}\n");
        assert_eq!(norm(&out.stderr), want, "{name}");
    }
}
