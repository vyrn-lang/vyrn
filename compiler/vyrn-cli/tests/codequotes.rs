//! Integration tests for RFC-0054 code quotes (`vyrn"…"`), driven through the
//! real `vyrn` binary:
//!
//!   * a code quote emits structurally-escaped source (`emit-gen`);
//!   * an injection attempt (a String carrying Vyrn syntax) is an inert literal;
//!   * a broken skeleton reports in the GENERATOR's file at the literal's line;
//!   * a bad identifier splice is a comptime error naming the generator;
//!   * `rawAt` origins round-trip: a check error inside the raw text maps back to
//!     the recorded path:line:col;
//!   * `std/scan` runs (the example) with the expected deterministic output.
//!
//! Generation runs with the cache disabled so a stale entry never masks a change.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn repo_file(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
        .canonicalize()
        .unwrap()
}

fn vyrn() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vyrn"));
    c.env("VYRN_NO_GEN_CACHE", "1");
    c
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn scratch(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_cq_{tag}_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
}

/// A generator + app pair in a fresh dir; returns the app path.
fn gen_app(tag: &str, gen: &str, app: &str) -> PathBuf {
    let dir = scratch(tag);
    write(&dir.join("gen.vyrn"), gen);
    write(&dir.join("app.vyrn"), app);
    dir.join("app.vyrn")
}

#[test]
fn emit_gen_emits_escaped_and_spliced_source() {
    let gen = "export gen fn mkMod(name: String) -> String {\n\
               let greeting = \"hi, \"\n\
               let body = vyrn\"\"\"export fn greet\\{name}(who: String) -> String {\n\
               return \\{greeting} + who\n\
               }\n\"\"\"\n\
               return render(body)\n\
               }\n";
    let app = "import { mkMod } from \"./gen\"\n\
               import { greetBob } from mkMod(\"Bob\")\n\
               fn main() -> Int64 { print(greetBob(\"x\")) return 0 }\n";
    let app_path = gen_app("emit", gen, app);
    let out = vyrn().arg("emit-gen").arg(&app_path).output().unwrap();
    assert!(
        out.status.success(),
        "emit-gen failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let src = String::from_utf8_lossy(&out.stdout);
    // The fragment splice built a derived name; the String splice became a literal.
    assert!(src.contains("export fn greetBob(who: String)"), "fragment splice:\n{src}");
    assert!(src.contains("return \"hi, \" + who"), "string→literal splice:\n{src}");
}

#[test]
fn injection_attempt_becomes_an_inert_string_literal() {
    let gen = "export gen fn mkMod(name: String) -> String {\n\
               let evil = \"\\\"; dropTables(); \\\"\"\n\
               let body = vyrn\"\"\"export fn \\{name}() -> String { return \\{evil} }\"\"\"\n\
               return render(body)\n\
               }\n";
    let app = "import { mkMod } from \"./gen\"\n\
               import { f } from mkMod(\"f\")\n\
               fn main() -> Int64 { print(f()) return 0 }\n";
    let app_path = gen_app("inj", gen, app);
    // Runs cleanly: the payload is DATA (a string value), never executed.
    let out = vyrn().arg("run").arg(&app_path).output().unwrap();
    assert!(
        out.status.success(),
        "run failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("; dropTables(); "), "payload printed as data:\n{stdout}");
}

#[test]
fn broken_skeleton_reports_in_the_generators_file() {
    // A typo in generator boilerplate: `type Query {` is GraphQL, not Vyrn — the
    // skeleton fails to parse and the error lands in gen.vyrn at the literal line.
    let gen = "export gen fn mkMod(name: String) -> String {\n\
               let body = vyrn\"\"\"\n\
               type Query {\n\
               }\n\"\"\"\n\
               return render(body)\n\
               }\n";
    let app = "import { mkMod } from \"./gen\"\n\
               import { x } from mkMod(\"x\")\n\
               fn main() -> Int64 { return 0 }\n";
    let app_path = gen_app("skel", gen, app);
    let out = vyrn().arg("check").arg(&app_path).output().unwrap();
    assert!(!out.status.success(), "expected a skeleton error");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("gen.vyrn"), "reported in the generator's file:\n{err}");
    assert!(err.contains("skeleton does not parse"), "skeleton message:\n{err}");
}

#[test]
fn bad_identifier_splice_names_the_generator() {
    let gen = "export gen fn mkMod(name: String) -> String {\n\
               let body = vyrn\"\"\"export fn \\{name}() -> Int64 { return 0 }\"\"\"\n\
               return render(body)\n\
               }\n";
    let app = "import { mkMod } from \"./gen\"\n\
               import { f } from mkMod(\"a b\")\n\
               fn main() -> Int64 { return 0 }\n";
    let app_path = gen_app("ident", gen, app);
    let out = vyrn().arg("check").arg(&app_path).output().unwrap();
    assert!(!out.status.success(), "expected an identifier-splice error");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("mkMod"), "names the generator:\n{err}");
    assert!(err.contains("\"a b\""), "quotes the offending value:\n{err}");
}

#[test]
fn rawat_origin_maps_a_check_error_back_to_the_source() {
    // The generator splices user text via `rawAt`, recording its origin. A type
    // error inside that text (`\"x\" + 1`) must be reported at the recorded
    // path:line:col, not at the generated module — the RFC-0033 round-trip.
    let dir = scratch("rawat");
    write(
        &dir.join("input.txt"),
        "placeholder\n", // origin file — line/col are what we assert against
    );
    let gen = "export gen fn mkMod(p: String) -> String {\n\
               let bad = rawAt(\"\\\"x\\\" + 1\", \"./input.txt\", 1, 5)\n\
               let body = vyrn\"\"\"export fn f() -> String {\n\
               return \\{bad}\n\
               }\"\"\"\n\
               return render(body)\n\
               }\n";
    write(&dir.join("gen.vyrn"), gen);
    let app = "import { mkMod } from \"./gen\"\n\
               import { f } from mkMod(\"./input.txt\")\n\
               fn main() -> Int64 { return 0 }\n";
    write(&dir.join("app.vyrn"), app);
    let out = vyrn()
        .arg("check")
        .arg(dir.join("app.vyrn"))
        .output()
        .unwrap();
    assert!(!out.status.success(), "expected a check error inside the raw text");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("input.txt:1:5"), "remapped to the origin:\n{err}");
    assert!(err.contains("generated code"), "keeps the generated location as a note:\n{err}");
}

#[test]
fn scan_example_runs_with_string_and_comment_awareness() {
    let ex = repo_file("examples/scan.vyrn");
    let out = vyrn().arg("run").arg(&ex).output().unwrap();
    assert!(
        out.status.success(),
        "scan example failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("ident: hello"), "{stdout}");
    // The comma inside the quoted string was not treated as a delimiter.
    assert!(stdout.contains("first: \"a, b\""), "string-aware `until`:\n{stdout}");
}
