//! Waiting code runs: each waiting example built with the free audit prints its
//! recorded stdout when the host delivers every wait, and frees every frame and
//! every value whichever wait the host cancels. Each prints the same bytes when
//! the host replays what it delivered. The programs under
//! `tests/waits/` witness `vyrn test`'s clock, a race the host's delivery order
//! decides, a waiting export's result on every host, and released and cancelled tasks on every
//! host.

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

/// Each cancellation ends the task at that wait, and the free audit finds nothing
/// left: every frame on the path took the cancellation edge, and every released
/// task freed its own. A cancelled `main` exits 1 once the tasks its released
/// handles left running end; a cancelled task no handle owns ends alone, and
/// `main` exits 0. Each line printed is a line of the full run.
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
            let full: Vec<&str> = all.lines().collect();
            assert!(
                printed.lines().all(|l| full.contains(&l)),
                "{name} wake {k}: {printed}"
            );
            assert_eq!(norm(&out.stderr), "", "{name} wake {k}");
            assert!(
                matches!(out.code, 0 | 1),
                "{name} wake {k}: exit {}",
                out.code
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

/// An `await` over a function value takes a function that waits and one that
/// does not, in a program that waits and in one that does not: a call into a
/// frame that does not wait finishes at its start.
#[test]
fn an_await_over_a_function_value_takes_any_function() {
    for (name, want) in [
        ("byvalue.vyrn", "206\n10\n203\n30\n"),
        ("byvalue_nowait.vyrn", "10\n30\n"),
    ] {
        let out = vyrn()
            .args(["run", name])
            .env("VYRN_LEAK_CHECK", "1")
            .current_dir(waits_dir())
            .output()
            .expect("run vyrn run");
        let seen = (out.status.code(), norm(&out.stdout), norm(&out.stderr));
        assert_eq!(seen, (Some(0), want.to_string(), String::new()), "{name}");
    }
}

/// The result types of the fork family: the type, the value a call returns (none for
/// `Unit`; a waiting `Int64` call returns its wait), how `show` prints a value `v`, and
/// what it prints.
type Forked = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
);
const FORKED: [Forked; 5] = [
    ("int", "Int64", "41", r#""\{v}""#, "41"),
    ("str", "String", r#""s" + "t""#, r#""\{v}""#, "st"),
    (
        "rec",
        "P",
        r#"P { x: 3, s: "r" + "c" }"#,
        r#""\{v.x} \{v.s}""#,
        "3 rc",
    ),
    (
        "opt",
        "Option<String>",
        r#"Some("o" + "p")"#,
        r#"match v { Some(s) => "\{s}", None => "none" }"#,
        "op",
    ),
    ("unit", "Unit", "", r#""unit""#, "unit"),
];

/// A program that forks two tasks over every ordered pair of [`FORKED`] types and waits for
/// them with `form`, and what it prints. The second task's wait is the shorter.
fn fork_family(form: &str) -> (String, String) {
    let mut src =
        "import { both, race, sleep, Race } from \"std/task\"\ntype P = { x: Int64, s: String }\n"
            .to_string();
    for (k, ty, val, show, _) in FORKED {
        let ret = |e: &str| match e {
            "" => String::new(),
            _ => format!("    return {e}\n"),
        };
        let val = if k == "int" { "ms" } else { val };
        src += &format!(
            "fn later{k}(ms: Int64) -> {ty} {{\n    await sleep(ms)\n{}}}\n",
            ret(val)
        );
        src += &format!("fn show{k}(v: {ty}) -> String {{\n    return {show}\n}}\n");
    }
    let (wa, wb) = (10, 5);
    let call = |k: &str, ms: i32| format!("fork later{k}({ms})");
    let shown = |(k, .., out): &Forked, ms: i32| match *k {
        "int" => ms.to_string(),
        _ => out.to_string(),
    };
    let (mut main, mut want) = (String::new(), String::new());
    for (x, y) in FORKED
        .iter()
        .flat_map(|x| FORKED.iter().map(move |y| (x, y)))
    {
        let (a, b) = (x.0, y.0);
        let (ca, cb) = (call(a, wa), call(b, wb));
        if form == "both" {
            main += &format!("    let r{a}{b} = await both({ca}, {cb})\n    print(show{a}(r{a}{b}.first) + \" \" + show{b}(r{a}{b}.second))\n");
            want += &format!("{} {}\n", shown(x, wa), shown(y, wb));
        } else {
            main += &format!("    match await race({ca}, {cb}) {{\n        First(v) => print(\"1 \" + show{a}(v)),\n        Second(v) => print(\"2 \" + show{b}(v)),\n    }}\n");
            want += &format!("2 {}\n", shown(y, wb));
        }
    }
    (
        format!("{src}fn main() -> Int64 {{\n{main}    return 0\n}}\n"),
        want,
    )
}

/// A `both` or a `race` over two tasks of any two result types builds and runs on the engine
/// and the native route, and frees every value.
#[test]
fn a_fork_over_any_two_result_types_runs_on_both_engines() {
    let dir = scratch("waits-forks");
    for form in ["both", "race"] {
        let (src, want) = fork_family(form);
        let file = dir.join(format!("{form}.vyrn"));
        std::fs::write(&file, &src).unwrap();
        let engine = vyrn()
            .env("VYRN_LEAK_CHECK", "1")
            .arg("run")
            .arg(&file)
            .output()
            .expect("run vyrn run");
        let seen = |o: &std::process::Output| (o.status.code(), norm(&o.stdout), norm(&o.stderr));
        assert_eq!(
            seen(&engine),
            (Some(0), want.clone(), String::new()),
            "{form}"
        );
        let exe = dir.join(format!("{form}.exe"));
        let built = vyrn()
            .env("VYRN_LEAK_CHECK", "1")
            .arg("build")
            .arg(&file)
            .arg("-o")
            .arg(&exe)
            .output()
            .expect("run vyrn build");
        assert!(built.status.success(), "{form}: {}", norm(&built.stderr));
        let native = std::process::Command::new(&exe)
            .output()
            .expect("run the binary");
        assert_eq!(seen(&native), seen(&engine), "{form} native");
    }
}

/// What `tests/waits/<name>` prints under `vyrn run`, with its exit code, once the native build
/// and, where node runs, the browser shim printed the same. Every build carries the free audit.
fn on_every_host(name: &str) -> String {
    let dir = scratch(&format!("waits-{}", name.trim_end_matches(".vyrn")));
    let say = |o: &std::process::Output| {
        let out = (o.status.code(), norm(&o.stdout), norm(&o.stderr));
        format!(
            "exit {:?}
{}{}",
            out.0, out.1, out.2
        )
    };
    let build = |target: &[&str], out: &std::path::Path| {
        let built = vyrn()
            .arg("build")
            .args(target)
            .args([name, "-o"])
            .arg(out)
            .env("VYRN_LEAK_CHECK", "1")
            .current_dir(waits_dir())
            .output()
            .expect("run vyrn build");
        assert!(built.status.success(), "{name}: {}", norm(&built.stderr));
    };
    let engine = vyrn()
        .args(["run", name])
        .env("VYRN_LEAK_CHECK", "1")
        .current_dir(waits_dir())
        .output()
        .expect("run vyrn run");
    let engine = say(&engine);
    let exe = dir.join("native.exe");
    build(&[], &exe);
    let native = std::process::Command::new(&exe)
        .output()
        .expect("run the binary");
    assert_eq!(say(&native), engine, "{name} native");
    let Some(node) = node() else {
        eprintln!("NOTE: no node, so the shim's run of {name} is unverified on this machine");
        return engine;
    };
    let wasm = dir.join("module.wasm");
    build(&["--target", "wasm"], &wasm);
    let web = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/wasi-min.js");
    std::fs::copy(web, dir.join("wasi-min.mjs")).expect("copy the shim");
    std::fs::write(dir.join("run.mjs"), SHIM_RUN).expect("write the runner");
    let shim = std::process::Command::new(node)
        .arg(dir.join("run.mjs"))
        .arg(&wasm)
        .output()
        .expect("node");
    assert_eq!(say(&shim), engine, "{name} shim");
    engine
}

/// A waiting export's result prints the same bytes on every host.
#[test]
fn a_waiting_export_s_result_prints_the_same_bytes_on_every_host() {
    assert_eq!(
        on_every_host("result.vyrn"),
        "exit Some(0)
10
after 3
0.500000
"
    );
}

/// A released handle leaves its task running, and `main`'s exit waits for it: a dropped
/// handle, a cleared array and a store over a handle, a `main` that ends while a task runs,
/// and a trap in a task no handle owns, which ends the program. `cancel` of a held handle
/// ends its task. Each frees every value on every host.
#[test]
fn a_released_task_runs_to_its_end_on_every_host() {
    for (name, want) in [
        (
            "released.vyrn",
            "exit Some(3)
main ends
the task runs on
",
        ),
        (
            "handles.vyrn",
            "exit Some(0)
d
joined d
b
a
c
",
        ),
        (
            "outlived.vyrn",
            "exit Some(0)
main ends
after main
",
        ),
        (
            "trapped.vyrn",
            "exit Some(1)
error: the released task trapped (trapped.vyrn:5)
",
        ),
        (
            "cancelled.vyrn",
            "exit Some(0)
u
joined u
",
        ),
    ] {
        assert_eq!(on_every_host(name), want, "{name}");
    }
}

/// Cancellation is explicit: a cancelled `timeout` cancels its branch in `std/task`'s code, and
/// a task cancelled under a plain await releases its handle, whose task runs on. Each frees
/// every value on every host.
#[test]
fn a_cancelled_task_cancels_only_what_its_code_cancels_on_every_host() {
    for (name, want) in [
        (
            "timedout.vyrn",
            "exit Some(0)
the branch printed nothing
",
        ),
        (
            "holder.vyrn",
            "exit Some(0)
the held task runs on
main ends
",
        ),
    ] {
        assert_eq!(on_every_host(name), want, "{name}");
    }
}

/// A trap runs no more code in its task, and the task it held runs on: under `vyrn test`, a
/// resident instance, the trap ends the holder alone. `vyrn run`, a native build and the shim
/// end the program at any trap, so this shape has no other host.
#[test]
fn a_trap_in_a_holder_leaves_its_held_task_running() {
    let out = vyrn()
        .args(["test", "trapholder.vyrn"])
        .env("VYRN_LEAK_CHECK", "1")
        .current_dir(waits_dir())
        .output()
        .expect("run vyrn test");
    assert!(
        out.status.success() && norm(&out.stdout).contains("1 passed, 0 failed"),
        "{}{}",
        norm(&out.stdout),
        norm(&out.stderr)
    );
}

/// A trap releases every handle its task held, as scope end would, and a handle a fork
/// argument moved belongs to the forked task: under `vyrn test`, a resident instance, each held
/// result is freed once, across fifty traps.
#[test]
fn a_trap_releases_the_handles_its_task_held() {
    let out = vyrn()
        .args(["test", "trapheld.vyrn"])
        .env("VYRN_LEAK_CHECK", "1")
        .current_dir(waits_dir())
        .output()
        .expect("run vyrn test");
    assert!(
        out.status.success() && norm(&out.stdout).contains("4 passed, 0 failed"),
        "{}{}",
        norm(&out.stdout),
        norm(&out.stderr)
    );
}

/// A fork of a call whose result crosses in its words (`Option<Int64>`, a record of two
/// `Int64`), or in the parameter it returns, lands the result in the task's block on every host.
#[test]
fn a_fork_of_a_call_whose_result_crosses_in_words_runs_on_every_host() {
    assert_eq!(
        on_every_host("forkwords.vyrn"),
        "exit Some(0)
1 2 xy 3 4 8
"
    );
}

/// A task cancelled while it waits in `joinOr` runs on with its child's handle and cancels the
/// child in its `Cancelled` arm. It frees every value on every host.
#[test]
fn a_cancel_aware_wait_answers_cancelled_on_every_host() {
    assert_eq!(
        on_every_host("aware.vyrn"),
        "exit Some(0)
the parent cancelled its child
main ends
"
    );
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

/// A group whose limit is below 1 traps where it is made, because `room`
/// would wait forever.
#[test]
fn a_group_with_no_room_traps_at_its_making() {
    let out = vyrn()
        .args(["run", "zerogroup.vyrn"])
        .current_dir(waits_dir())
        .output()
        .expect("run vyrn run");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(norm(&out.stdout), "");
    assert_eq!(
        norm(&out.stderr),
        "error: validation failed for parameter `limit` of `group`\n"
    );
}
