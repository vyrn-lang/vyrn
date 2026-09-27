//! Code quotes driven through the `vyrn` binary. Generation runs
//! with the cache disabled so a stale entry never masks a change.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

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

/// Writes a generator and an app to a fresh directory; returns the app path.
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
    assert!(
        src.contains("export fn greetBob(who: String)"),
        "fragment splice:\n{src}"
    );
    assert!(
        src.contains("return \"hi, \" + who"),
        "string→literal splice:\n{src}"
    );
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
    let out = vyrn().arg("run").arg(&app_path).output().unwrap();
    assert!(
        out.status.success(),
        "run failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("; dropTables(); "),
        "payload printed as data:\n{stdout}"
    );
}

#[test]
fn broken_skeleton_reports_in_the_generators_file() {
    // `type Query {` is GraphQL, not Vyrn, so the skeleton does not parse.
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
    assert!(
        err.contains("gen.vyrn"),
        "reported in the generator's file:\n{err}"
    );
    assert!(
        err.contains("skeleton does not parse"),
        "skeleton message:\n{err}"
    );
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
    assert!(
        err.contains("\"a b\""),
        "quotes the offending value:\n{err}"
    );
}

#[test]
fn rawat_origin_maps_a_check_error_back_to_the_source() {
    // The error must land at the origin `rawAt` recorded, not in the generated
    // module.
    let dir = scratch("rawat");
    write(
        &dir.join("input.txt"),
        "placeholder\n", // the origin `rawAt` records
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
    assert!(
        !out.status.success(),
        "expected a check error inside the raw text"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("input.txt:1:5"),
        "remapped to the origin:\n{err}"
    );
    assert!(
        err.contains("generated code"),
        "keeps the generated location as a note:\n{err}"
    );
}

#[test]
fn i18n_translation_with_quotes_and_backslashes_bakes_losslessly() {
    // The i18n generator bakes each translation through the `strLit` code quote,
    // so a quote or a backslash in the value must round-trip byte for byte.
    let dir = scratch("i18n_esc");
    std::fs::create_dir_all(dir.join("locales")).unwrap();
    write(
        &dir.join("locales/en.json"),
        r#"{ "quote.msg": "say \"hi\" \\ ok" }"#,
    );
    let app = "import { i18n } from \"std/i18n\"\n\
               import { tQuoteMsg } from i18n(\"./locales\")\n\
               fn main() -> Int64 { print(tQuoteMsg()) return 0 }\n";
    write(&dir.join("app.vyrn"), app);
    let out = vyrn()
        .arg("run")
        .arg(dir.join("app.vyrn"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "run failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(r#"say "hi" \ ok"#),
        "lossless bake:\n{stdout}"
    );
}

/// A splice's tag goes on the stack before its value, so the value a field read
/// binds cannot ride the stack into it.
#[test]
fn a_spliced_field_read_is_not_carried_under_the_tag() {
    let gen = "type F = { box: String, n: Int64 }\n\
               export gen fn mk(a: String) -> String {\n\
               let found = F { box: a + \"x\", n: 1 }\n\
               let r = render(vyrn\"\\{found.box}\")\n\
               return \"export fn got() -> Int64 { return \\{r.byteLength} }\\n\"\n\
               }\n";
    let app = "import { mk } from \"./gen\"\n\
               import { got } from mk(\"ab\")\n\
               fn main() -> Int64 { print(got().toString()) return 0 }\n";
    let app_path = gen_app("splicefield", gen, app);
    let out = vyrn().arg("run").arg(&app_path).output().unwrap();
    assert!(
        out.status.success(),
        "run failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "5");
}
