//! The generator diagnostic channel, through the `vyrn`
//! binary.
//!
//! A warning rides a load that succeeded, so it changes neither the exit code
//! nor a byte of program output, except under `--deny-warnings`. An `Error`
//! report stops the build. The fixture is a purpose-built generator: no real
//! generator emits a warning, so these tests are the channel's only proof.

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
    // path joining cannot parse (84b78d8).
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

/// Generators emitting a module with a report whose anchor is an input
/// position.
const GEN_WARNING: &str = r#"import { report, reportHere, Severity } from "std/diag"

gen fn body() -> Code {
    return vyrn"export fn greeting() -> String {\n    return \"hi\"\n}\n"
}

export gen fn legacy(path: String) -> Code {
    return report(Warning, path, 2, 1, "`fn old` is deprecated — write `fn new` instead") + body()
}

/// The same module with nothing to say, for the byte-for-byte comparison.
export gen fn quiet(path: String) -> Code {
    return body()
}

/// A report with no source position to give.
export gen fn unpositioned(path: String) -> Code {
    return reportHere(Warning, "`fn old` is deprecated") + body()
}

/// A report whose message spans two lines and whose position is below 1.
export gen fn clamped(path: String) -> Code {
    return report(Warning, path, 0, 0, "two\nlines") + body()
}

/// A report the generator says must STOP the build. The module it
/// emits is perfectly valid — the refusal is the generator's judgement about its
/// input, not a fault the compiler could have found.
export gen fn refuses(path: String) -> Code {
    return report(Error, path, 2, 1, "`fn old` has no replacement — remove the call") + body()
}
"#;

/// The file the directive points at; the generator never reads it.
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

/// A report is one line, and a position below 1 is clamped to 1, not lost.
#[test]
fn a_report_is_one_line_at_a_clamped_position() {
    let r = run("clamped", "clamped", "run", &[], &[]);
    assert_eq!(r.code, 0, "the load SUCCEEDED:\n{}", r.stderr);
    assert!(
        r.stderr.contains("legacy.vyrn:1:1: warning: two lines"),
        "{}",
        r.stderr
    );
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
    // The author never sees the generated module, so a warning against it
    // would be unactionable.
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
    // The invariant the channel exists to protect.
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
    // Read and stripped like `--offline`, so CI can set it once.
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
    // One print site in `load_program`: no command may reach the loader by
    // another road.
    for cmd in ["check", "run", "emit-wat"] {
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

#[test]
fn an_error_severity_fails_the_load_in_the_generators_own_words() {
    // A generator refuses, and says why, instead
    // of synthesizing an identifier that fails to resolve and hoping the
    // parser's wording lands close enough.
    let r = run("refuses", "refuses", "run", &[], &[]);
    assert_ne!(r.code, 0, "the load FAILED:\n{}", r.stderr);
    assert_eq!(r.stdout, "", "the program never ran");
    assert!(
        r.stderr
            .contains("legacy.vyrn:2:1: `fn old` has no replacement — remove the call"),
        "anchored at the input file, at the line and column the generator gave:\n{}",
        r.stderr
    );
    assert!(
        r.stderr.contains("note: in generated code"),
        "and the generated location survives as a note:\n{}",
        r.stderr
    );
    assert!(
        !r.stderr.contains('\u{1f}'),
        "the note spells the banner's separator as ` at ` (#589):\n{}",
        r.stderr
    );
}

#[test]
fn a_generator_error_needs_no_deny_warnings_to_bite() {
    // The severity is the generator's decision: no flag is needed.
    for cmd in ["check", "run", "emit-wat"] {
        let r = run(&format!("refuse_{cmd}"), "refuses", cmd, &[], &[]);
        assert_ne!(r.code, 0, "`vyrn {cmd}` refused:\n{}", r.stderr);
        assert!(
            r.stderr.contains("`fn old` has no replacement"),
            "`vyrn {cmd}` says why:\n{}",
            r.stderr
        );
    }
}
