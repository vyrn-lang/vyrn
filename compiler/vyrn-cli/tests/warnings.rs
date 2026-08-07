//! Integration tests for the warning channel (RFC-0071 M2b), driven through the
//! real `vyrn` binary.
//!
//! Until M2b nothing in the front end ever produced a `Severity::Warning`:
//! `load()` returns `Result<Program, Vec<Diagnostic>>`, so there was no
//! success-path diagnostic at all, and a generator's only way to say anything
//! was an identifier line that fails to parse — fatal by construction, which is
//! the opposite of a notice you are meant to read and keep going.
//!
//! The channel's whole contract is in the name: a warning rides a load that
//! SUCCEEDED. So the assertions here are mostly about what must *not* change —
//! the exit code, and every byte of the program's own output — plus the one
//! switch that deliberately does change it (`--deny-warnings`), which is what
//! lets CI refuse a build that compiled with something left to say.
//!
//! The fixture is a purpose-built generator rather than a real `.vyx` page, and
//! that is why this file still passes: RFC-0071 M2c deleted the deprecation
//! notices that were the channel's only real-world producer, and the channel is
//! retained on its own merits — a compiler wants a way to say "this compiled,
//! but". These tests are that capability's only proof.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn repo_dir(rel: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
        .canonicalize()
        .unwrap();
    // Windows `canonicalize` returns a `\\?\` verbatim path, which the loader's
    // own path joining cannot parse (see 84b78d8) — strip it.
    let s = p.to_string_lossy().replace('\\', "/");
    PathBuf::from(s.strip_prefix("//?/").unwrap_or(&s).to_string())
}

fn vyrn() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vyrn"));
    c.env("VYRN_NO_GEN_CACHE", "1");
    c.env("VYRN_STD", repo_dir("std"));
    // The switch is read from the environment, so an inherited one would make
    // every "warnings change nothing" assertion vacuous.
    c.env_remove("VYRN_DENY_WARNINGS");
    c
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn scratch(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_warn_{tag}_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A generator that emits a module carrying a `//@warning` directive whose
/// leading field is an origin position — a generator saying something about an
/// input file without failing the build.
const GEN_WARNING: &str = r#"export gen fn legacy(path: String) -> String {
    return "//@warning " + path + ":2:1 `fn old` is deprecated — write `fn new` instead\n" +
        "export fn greeting() -> String {\n    return \"hi\"\n}\n"
}

/// The same module with nothing to say, for the byte-for-byte comparison.
export gen fn quiet(path: String) -> String {
    return "export fn greeting() -> String {\n    return \"hi\"\n}\n"
}

/// A generator with no source position to give writes `-` there.
export gen fn unpositioned(path: String) -> String {
    return "//@warning - `fn old` is deprecated\n" +
        "export fn greeting() -> String {\n    return \"hi\"\n}\n"
}
"#;

/// The file the directive points at. Its contents are never read by the
/// generator — the point is that the warning names a file the author wrote.
const LEGACY: &str = r#"// A module on the old form.
fn old() -> Int64 {
    return 1
}
"#;

fn app_for(genfn: &str) -> String {
    format!(
        "import {{ {genfn} }} from \"./gen\"\n\
         import {{ greeting }} from {genfn}(\"./legacy.vyrn\")\n\
         \n\
         fn main() -> Int64 {{\n    print(greeting())\n    return 0\n}}\n"
    )
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

/// Build the fixture and run `vyrn <cmd>` over it with `extra` flags.
fn run(tag: &str, genfn: &str, cmd: &str, extra: &[&str], env: &[(&str, &str)]) -> Run {
    let dir = scratch(tag);
    std::fs::write(dir.join("gen.vyrn"), GEN_WARNING).unwrap();
    std::fs::write(dir.join("legacy.vyrn"), LEGACY).unwrap();
    std::fs::write(dir.join("app.vyrn"), app_for(genfn)).unwrap();
    let mut c = vyrn();
    c.arg(cmd).arg(dir.join("app.vyrn"));
    for a in extra {
        c.arg(a);
    }
    for (k, v) in env {
        c.env(k, v);
    }
    let out = c.output().expect("run vyrn");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
        stderr: String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n"),
    }
}

#[test]
fn a_warning_rides_a_successful_load() {
    let r = run("rides", "legacy", "run", &[], &[]);
    assert_eq!(r.code, 0, "the load SUCCEEDED:\n{}", r.stderr);
    assert_eq!(
        r.stdout, "hi\n",
        "the program ran and printed its own output"
    );
    assert!(
        r.stderr
            .contains("warning: `fn old` is deprecated — write `fn new` instead"),
        "the notice reaches the user as a warning:\n{}",
        r.stderr
    );
}

#[test]
fn a_warning_points_at_the_users_source_line_not_the_generated_text() {
    // The whole reason the directive carries a position: the author never sees
    // the generated module, so a warning against it would be unactionable.
    let r = run("position", "legacy", "run", &[], &[]);
    assert!(
        r.stderr.contains("legacy.vyrn:2:1: warning:"),
        "reported at the input file, line 2, column 1:\n{}",
        r.stderr
    );
    assert!(
        !r.stderr.contains("generated by legacy") || r.stderr.contains("note:"),
        "the generated location survives only as a note:\n{}",
        r.stderr
    );
    assert!(
        r.stderr.contains("note: in generated code"),
        "and it is not lost:\n{}",
        r.stderr
    );
}

#[test]
fn an_unpositioned_warning_does_not_speak_its_placeholder() {
    // `-` is the "no position" marker, not the first word of the message.
    let r = run("unpositioned", "unpositioned", "run", &[], &[]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(
        r.stderr.contains("warning: `fn old` is deprecated"),
        "the marker is consumed:\n{}",
        r.stderr
    );
    assert!(
        !r.stderr.contains("warning: - "),
        "and never printed:\n{}",
        r.stderr
    );
}

#[test]
fn warnings_change_neither_the_exit_code_nor_a_byte_of_program_output() {
    // The invariant the channel exists to protect. Two identical programs, one
    // whose generator also emits a warning: same status, same stdout.
    let warned = run("same_warned", "legacy", "run", &[], &[]);
    let quiet = run("same_quiet", "quiet", "run", &[], &[]);
    assert_eq!(warned.code, quiet.code, "same exit code");
    assert_eq!(warned.stdout, quiet.stdout, "byte-identical stdout");
    assert!(
        quiet.stderr.is_empty(),
        "the quiet run says nothing: {}",
        quiet.stderr
    );
    assert!(!warned.stderr.is_empty(), "the warned one does");
}

#[test]
fn deny_warnings_flips_a_warned_load_to_a_failure() {
    let r = run("deny", "legacy", "run", &["--deny-warnings"], &[]);
    assert_ne!(r.code, 0, "refused:\n{}", r.stderr);
    assert!(
        r.stderr.contains("refused by --deny-warnings"),
        "and says why:\n{}",
        r.stderr
    );
    assert!(
        r.stderr.contains("warning: `fn old` is deprecated"),
        "the warning itself is still printed:\n{}",
        r.stderr
    );
    assert_eq!(r.stdout, "", "the program never ran");
}

#[test]
fn deny_warnings_leaves_a_clean_load_alone() {
    let r = run("deny_clean", "quiet", "run", &["--deny-warnings"], &[]);
    assert_eq!(r.code, 0, "nothing to refuse:\n{}", r.stderr);
    assert_eq!(r.stdout, "hi\n");
}

#[test]
fn the_environment_variable_is_the_same_switch() {
    // Spelled and stripped exactly like `--offline`, so CI can set it once.
    let r = run(
        "deny_env",
        "legacy",
        "run",
        &[],
        &[("VYRN_DENY_WARNINGS", "1")],
    );
    assert_ne!(r.code, 0, "refused:\n{}", r.stderr);
    assert!(
        r.stderr.contains("refused by --deny-warnings"),
        "{}",
        r.stderr
    );
}

#[test]
fn every_command_that_builds_a_program_prints_the_warning() {
    // One print site in `load_program`, so this is really a test that no command
    // reaches the loader by some other road.
    for cmd in ["check", "run", "emit-ir"] {
        let r = run(&format!("cmd_{cmd}"), "legacy", cmd, &[], &[]);
        assert_eq!(r.code, 0, "{cmd} succeeded:\n{}", r.stderr);
        assert!(
            r.stderr.contains("warning: `fn old` is deprecated"),
            "`vyrn {cmd}` prints it:\n{}",
            r.stderr
        );
    }
}

#[test]
fn a_failing_load_reports_the_failure_and_not_the_advice() {
    // A program that did not compile gets errors, not advisory notes: the
    // output stays about the thing that went wrong.
    let dir = scratch("failing");
    std::fs::write(dir.join("gen.vyrn"), GEN_WARNING).unwrap();
    std::fs::write(dir.join("legacy.vyrn"), LEGACY).unwrap();
    std::fs::write(
        dir.join("app.vyrn"),
        "import { legacy } from \"./gen\"\n\
         import { greeting } from legacy(\"./legacy.vyrn\")\n\
         \n\
         fn main() -> Int64 {\n    print(nope())\n    return 0\n}\n",
    )
    .unwrap();
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .expect("run vyrn");
    let stderr = String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n");
    assert!(!out.status.success(), "the load failed: {stderr}");
    assert!(
        !stderr.contains("warning:"),
        "no advice on a failure:\n{stderr}"
    );
}
