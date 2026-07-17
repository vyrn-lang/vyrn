//! Integration tests for theme-derived utility classes (RFC-0032) — the `std/tw`
//! generator driven through the real `vyrn` binary.
//!
//!   * `emit-gen` the demo and assert the synthesized module's shape (TwClass /
//!     Tw finite/regex types, `cls`, and a baked `css()`);
//!   * a typo'd class literal fails `vyrn check` with the validated-type
//!     diagnostic (the compile-error demonstration RFC-0032 asks for);
//!   * malformed theme.json (unknown key, non-string leaf, unsafe name) fails the
//!     load with a pointed, key-naming diagnostic;
//!   * the demo runs green under `vyrn test`.
//!
//! Generation runs with the cache disabled so a stale entry never masks a
//! regression.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn repo_file(rel: &str) -> PathBuf {
    // vyrn-cli/ -> compiler/ -> repo root
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel).canonicalize().unwrap()
}

fn vyrn() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vyrn"));
    c.env("VYRN_NO_GEN_CACHE", "1");
    c
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A fresh, empty scratch directory for a test's fixtures.
fn scratch(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_tw_{tag}_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
}

/// A small, valid theme covering every axis.
const GOOD_THEME: &str = r##"{
  "colors":  { "brand": { "500": "#4f46e5", "600": "#4338ca" }, "white": "#ffffff" },
  "spacing": { "1": "0.25rem", "2": "0.5rem" },
  "radius":  { "DEFAULT": "0.5rem" },
  "fontSize": { "base": "1rem" },
  "breakpoints": { "md": "768px" }
}"##;

// ---- emit-gen: the synthesized module's shape ------------------------------

#[test]
fn emit_gen_shows_the_synthesized_theme_module() {
    let demo = repo_file("examples/twdemo.vyrn");
    let out = vyrn().arg("emit-gen").arg(&demo).output().expect("emit-gen");
    assert!(out.status.success(), "emit-gen failed:\n{}", String::from_utf8_lossy(&out.stderr));
    let src = String::from_utf8_lossy(&out.stdout);

    // The bridge imports std/html for `Attr`/`Cls`.
    assert!(src.contains("import { Attr } from \"std/html\""), "html import:\n{}", &src[..src.len().min(400)]);

    // TwClass — the finite single-class type, factoring the bounded prefixes over
    // the base-class alternation.
    assert!(
        src.contains("export type TwClass = String where value =~ \"((md:)?(hover:|focus:)?("),
        "TwClass finite type header not found"
    );
    // Tw — a space-separated sequence of single classes (`class( class)*`).
    assert!(src.contains("export type Tw = String where value =~ \"("), "Tw type");
    assert!(src.contains(")( ("), "Tw sequence loop (space-separated repetition)");

    // A few vocabulary members, in the RFC-locked family order.
    assert!(src.contains("bg-brand-500|"), "colour class");
    assert!(src.contains("|p-1|p-2|"), "spacing class");
    assert!(src.contains("|rounded|"), "radius class");
    assert!(src.contains("|flex|"), "static utility");

    // The checked bridge and the baked stylesheet.
    assert!(src.contains("export fn cls(c: Tw) -> Attr"), "cls bridge");
    assert!(src.contains("export fn css() -> String"), "css()");
    // css() is a baked constant carrying real rules (escaped `\:` in the literal).
    assert!(src.contains(".bg-brand-500 {background-color:#4f46e5}"), "base rule baked");
    assert!(src.contains("@media (min-width:768px) {"), "media block baked");
}

// ---- the compile-error demonstration (a typo'd class literal) --------------

#[test]
fn a_typoed_class_literal_fails_check() {
    let dir = scratch("typo");
    write(&dir.join("theme.json"), GOOD_THEME);
    write(
        &dir.join("app.vyrn"),
        "import { tw } from \"std/tw\"\n\
         import * as theme from tw(\"./theme.json\")\n\
         import { Attr } from \"std/html\"\n\
         fn main() -> Int64 {\n\
         let c: Attr = theme.cls(\"px-2 bg-brnd-500\")\n\
         return 0\n\
         }\n",
    );
    let out = vyrn().arg("check").arg(dir.join("app.vyrn")).output().expect("check");
    assert!(!out.status.success(), "a typo'd class literal must fail `vyrn check`");
    let err = String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    // The validated-type diagnostic names the offending literal and the `Tw` type.
    assert!(err.contains("does not satisfy `Tw`"), "expected the Tw validation diagnostic, got:\n{err}");
    assert!(err.contains("bg-brnd-500"), "diagnostic should quote the bad literal:\n{err}");
}

// ---- a good literal checks clean --------------------------------------------

#[test]
fn a_valid_class_literal_checks_clean() {
    let dir = scratch("good");
    write(&dir.join("theme.json"), GOOD_THEME);
    write(
        &dir.join("app.vyrn"),
        "import { tw } from \"std/tw\"\n\
         import * as theme from tw(\"./theme.json\")\n\
         import { Attr } from \"std/html\"\n\
         fn main() -> Int64 {\n\
         let c: Attr = theme.cls(\"px-2 rounded bg-brand-500 md:hover:bg-brand-600\")\n\
         return 0\n\
         }\n",
    );
    let out = vyrn().arg("check").arg(dir.join("app.vyrn")).output().expect("check");
    assert!(
        out.status.success(),
        "a valid multi-token literal (incl. an md:hover: variant) must check clean:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ---- malformed theme.json fails generation ---------------------------------

#[test]
fn an_unknown_theme_key_fails_generation() {
    let dir = scratch("unknownkey");
    write(&dir.join("theme.json"), "{ \"shadows\": { \"lg\": \"0 1px 2px\" } }");
    write(
        &dir.join("app.vyrn"),
        "import { tw } from \"std/tw\"\n\
         import * as theme from tw(\"./theme.json\")\n\
         fn main() -> Int64 { return 0 }\n",
    );
    let out = vyrn().arg("run").arg(dir.join("app.vyrn")).output().expect("run");
    assert!(!out.status.success(), "an unknown theme key must fail to load");
    let err = String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(err.contains("TW_UNKNOWN_KEY__shadows"), "unknown-key diagnostic:\n{err}");
}

#[test]
fn a_non_string_leaf_fails_generation() {
    let dir = scratch("badleaf");
    write(&dir.join("theme.json"), "{ \"spacing\": { \"2\": 8 } }");
    write(
        &dir.join("app.vyrn"),
        "import { tw } from \"std/tw\"\n\
         import * as theme from tw(\"./theme.json\")\n\
         fn main() -> Int64 { return 0 }\n",
    );
    let out = vyrn().arg("run").arg(dir.join("app.vyrn")).output().expect("run");
    assert!(!out.status.success(), "a non-string leaf must fail to load");
    let err = String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(err.contains("TW_PARSE_ERROR__"), "parse-error diagnostic:\n{err}");
}

#[test]
fn an_unsafe_class_name_fails_generation() {
    let dir = scratch("unsafe");
    // An uppercase colour name yields `bg-Brand-500`, not `[a-z][a-z0-9-]*`.
    write(&dir.join("theme.json"), "{ \"colors\": { \"Brand\": { \"500\": \"#abc\" } } }");
    write(
        &dir.join("app.vyrn"),
        "import { tw } from \"std/tw\"\n\
         import * as theme from tw(\"./theme.json\")\n\
         fn main() -> Int64 { return 0 }\n",
    );
    let out = vyrn().arg("run").arg(dir.join("app.vyrn")).output().expect("run");
    assert!(!out.status.success(), "an unsafe class name must fail to load");
    let err = String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(err.contains("TW_UNSAFE_NAME__"), "unsafe-name diagnostic:\n{err}");
}

// ---- the demo runs green ---------------------------------------------------

#[test]
fn demo_tests_run_green() {
    let demo = repo_file("examples/twdemo.vyrn");
    let out = vyrn().arg("test").arg(&demo).output().expect("vyrn test");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "demo tests failed:\n{combined}");
    assert!(combined.contains("3 passed, 0 failed"), "expected 3 green tests:\n{combined}");
}

// ---- std/tw's own unit tests run green -------------------------------------

#[test]
fn std_tw_unit_tests_run_green() {
    let module = repo_file("std/tw.vyrn");
    let out = vyrn().arg("test").arg(&module).output().expect("vyrn test");
    let combined =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "std/tw unit tests failed:\n{combined}");
    assert!(combined.contains("11 passed, 0 failed"), "expected 11 green tests:\n{combined}");
}
