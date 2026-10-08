//! Holds every output of the checker and the placer independent of how many
//! threads type, build and place bodies, and of the order they take them in
//! (`VYRN_THREADS`, `VYRN_SHUFFLE`). Each root is checked, reported by `vyrn
//! why --cost`, printed by `vyrn emit-lowered` and built to wasm, and every
//! byte must equal the one-thread run's: the typing refusals, the cost rows,
//! the placed releases and the facts the emitter reads.

mod common;
use common::*;
use std::path::Path;

/// The corpus's largest roots by lines read, and the root the checker
/// refuses most often: 170 lines of refusals across its bodies.
const ROOTS: &[&str] = &[
    "site/export.vyrn",
    "site/app/docshell.vyrn",
    "site/app/search.vyrn",
    "examples/pagesdemo.vyrn",
    "examples/graphql.vyrn",
    "examples/rest.vyrn",
    "compiler/vyrn-cli/tests/checker-rules.vyrn",
];

/// `(VYRN_THREADS, VYRN_SHUFFLE)`. The first run is the reference.
const RUNS: &[(&str, Option<&str>)] = &[("1", None), ("8", None), ("8", Some("7"))];

/// The transcript of every command over `file`, and the wasm it built to
/// `wasm`, a path every run shares: `vyrn build` prints it.
fn outputs(file: &str, wasm: &Path, (threads, shuffle): (&str, Option<&str>)) -> (String, Vec<u8>) {
    let root = examples_dir().parent().unwrap().to_path_buf();
    let _ = std::fs::remove_file(wasm);
    let wasm_arg = wasm.to_string_lossy().into_owned();
    let mut text = String::new();
    for args in [
        vec!["check", file],
        vec!["why", "--cost", file],
        vec!["emit-lowered", file],
        vec!["build", file, "--target", "wasm", "-o", &wasm_arg],
    ] {
        let mut cmd = vyrn();
        cmd.current_dir(&root)
            .args(&args)
            .env("VYRN_THREADS", threads);
        match shuffle {
            Some(seed) => cmd.env("VYRN_SHUFFLE", seed),
            None => cmd.env_remove("VYRN_SHUFFLE"),
        };
        let o = cmd.output().expect("run vyrn");
        text.push_str(&format!("$ vyrn {} -> {:?}\n", args[0], o.status.code()));
        text.push_str(&norm(&o.stdout));
        text.push_str(&norm(&o.stderr));
    }
    (text, std::fs::read(wasm).unwrap_or_default())
}

#[test]
#[ignore = "runs the largest roots on one thread and many; run explicitly: cargo test -p vyrn-cli --test parallel -- --ignored"]
fn every_output_is_the_same_on_one_thread_many_and_a_shuffled_order() {
    let mut failures = Vec::new();
    for file in ROOTS {
        let out = scratch("parallel");
        let wasm = out.join("out.wasm");
        let (want, want_wasm) = outputs(file, &wasm, RUNS[0]);
        for &run in &RUNS[1..] {
            let (got, got_wasm) = outputs(file, &wasm, run);
            let name = format!("{run:?}");
            if let Some(d) = first_diff(file, "1 thread", &want, &name, &got) {
                failures.push(d);
            }
            if got_wasm != want_wasm {
                failures.push(format!("  {file}: the wasm differs under {name}\n"));
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
